use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use coblet_runtime::{
    ArtifactKind, Expectation, HttpUrl, RelPath, Request, Storage, StreamHasher, Transport,
    UrlRewriter, UrlRewriters, VerifyError, Warning, WarningCode,
};
use coblet_types::FileHash;
use serde::{Deserialize, Serialize};

use crate::CatalogError;

/// Warning code published when cached metadata is used because the network failed.
pub const OFFLINE_CACHE: WarningCode = WarningCode::from_static("catalog.offline-cache");

/// Warning code published when a trusted mirror's metadata document is
/// used although it does not match its expected hash.
pub const UNVERIFIED_MIRROR: WarningCode = WarningCode::from_static("catalog.unverified-mirror");

/// Largest metadata document read into memory by default (64 MiB).
pub const DEFAULT_MAX_SIZE: u64 = 64 * 1024 * 1024;

/// How a [`MetaCache`] uses a cached copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CachePolicy {
    /// Ask the server whether the cached copy is current (`If-None-Match`,
    /// `If-Modified-Since`) and fall back to it when the network fails.
    #[default]
    Revalidate,
    /// Use a cached copy without asking the server; fetch only when there is none.
    PreferCache,
    /// Fetch the document again without conditional headers; still fall back
    /// to the cached copy when the network fails.
    Refresh,
}

/// Where a document returned by [`MetaCache::fetch`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The server sent the document.
    Network,
    /// The server confirmed the cached copy is current.
    NotModified,
    /// The cached copy was used without asking the server.
    Cache,
    /// The network failed and the cached copy was used; a warning was recorded.
    OfflineCache,
}

/// One metadata document to fetch.
///
/// # Examples
///
/// ```
/// use coblet_catalog::MetaRequest;
/// use coblet_runtime::{HttpUrl, RelPath};
///
/// let request = MetaRequest::new(
///     HttpUrl::new("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")?,
///     RelPath::new("mojang/version_manifest_v2.json")?,
/// );
/// assert!(request.expect.is_none());
/// assert_eq!(request.kind, coblet_runtime::ArtifactKind::Metadata);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaRequest {
    /// The address.
    pub url: HttpUrl,
    /// Path of the cached copy under the cache root.
    pub key: RelPath,
    /// Hash the document must match. A cached copy that matches is used
    /// without a request, because its content cannot change.
    pub expect: Option<FileHash>,
    /// Largest accepted document.
    pub max_size: u64,
    /// What the document is, for [`UrlRewriter`]s; [`ArtifactKind::Metadata`] by default.
    pub kind: ArtifactKind,
}

impl MetaRequest {
    /// Creates a request without an expected hash.
    #[must_use]
    pub fn new(url: HttpUrl, key: RelPath) -> Self {
        Self {
            url,
            key,
            expect: None,
            max_size: DEFAULT_MAX_SIZE,
            kind: ArtifactKind::Metadata,
        }
    }

    /// Marks the document as another kind, such as [`ArtifactKind::Loader`].
    #[must_use]
    pub fn with_kind(self, kind: ArtifactKind) -> Self {
        Self { kind, ..self }
    }

    /// Requires the document to match a hash.
    #[must_use]
    pub fn with_hash(self, expect: FileHash) -> Self {
        Self {
            expect: Some(expect),
            ..self
        }
    }
}

/// Document returned by [`MetaCache::fetch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The document.
    pub body: Bytes,
    /// Where it came from.
    pub origin: Origin,
    /// Set when the cached copy was used because the network failed.
    pub warning: Option<Warning>,
}

/// Validators stored next to a cached document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Validators {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
    /// Set when the document was accepted from a trusted mirror although it
    /// did not match the expected hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accepted: Option<Accepted>,
}

impl Validators {
    fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none() && self.accepted.is_none()
    }
}

/// Hashes of a document accepted without matching its expected hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Accepted {
    expected: String,
    actual: String,
}

/// Why one address did not give a usable document.
struct Failed {
    error: CatalogError,
    /// The document, when the only problem was its hash.
    mismatch: Option<Box<Mismatch>>,
}

struct Mismatch {
    body: Bytes,
    validators: Validators,
    expected: FileHash,
    actual: FileHash,
}

impl From<CatalogError> for Failed {
    fn from(error: CatalogError) -> Self {
        Self {
            error,
            mismatch: None,
        }
    }
}

/// HTTP cache for metadata documents, usually rooted at `data_root/cache/meta`.
///
/// Each document is stored at its key with a `<key>.http.json` file holding
/// the `ETag` and `Last-Modified` validators. Version manifests, version
/// JSON files and Mod Loader indexes all go through it, so the launcher can
/// list versions without a network connection.
///
/// [`UrlRewriter`]s change the address a document is requested from, not
/// where it is cached, so switching mirrors keeps the cache.
#[derive(Clone)]
pub struct MetaCache {
    transport: Arc<dyn Transport>,
    storage: Arc<dyn Storage>,
    root: PathBuf,
    rewriters: UrlRewriters,
}

impl std::fmt::Debug for MetaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaCache")
            .field("root", &self.root)
            .field("rewriters", &self.rewriters)
            .finish_non_exhaustive()
    }
}

fn check(body: &[u8], expect: &FileHash) -> Result<(), VerifyError> {
    let expectation = Expectation::hash(expect.clone(), None);
    let mut hasher = StreamHasher::for_expectation(&expectation);
    hasher.update(body);
    hasher.finish().check(&expectation)
}

impl MetaCache {
    /// Creates a cache rooted at `root`.
    #[must_use]
    pub fn new(
        transport: Arc<dyn Transport>,
        storage: Arc<dyn Storage>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            transport,
            storage,
            root: root.into(),
            rewriters: UrlRewriters::new(),
        }
    }

    /// Adds a rewriter; rewriters run in the order they were added.
    #[must_use]
    pub fn with_rewriter(self, rewriter: Arc<dyn UrlRewriter>) -> Self {
        Self {
            rewriters: self.rewriters.with(rewriter),
            ..self
        }
    }

    /// Replaces the rewriter chain, for example with one shared with a downloader.
    #[must_use]
    pub fn with_rewriters(self, rewriters: UrlRewriters) -> Self {
        Self { rewriters, ..self }
    }

    /// Returns the rewriter chain.
    #[must_use]
    pub fn rewriters(&self) -> &UrlRewriters {
        &self.rewriters
    }

    /// Returns the cache root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the path of a cached document.
    #[must_use]
    pub fn path_of(&self, key: &RelPath) -> PathBuf {
        key.under(&self.root)
    }

    fn validators_path(&self, key: &RelPath) -> PathBuf {
        let mut path = key.under(&self.root).into_os_string();
        path.push(".http.json");
        PathBuf::from(path)
    }

    async fn read_optional(&self, path: &Path) -> Result<Option<Bytes>, CatalogError> {
        match self.storage.read(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns a document, from the network or the cache as `policy` allows.
    ///
    /// The request goes to the address the rewriter chain chooses. When a
    /// rewritten address fails or serves a document that does not match
    /// the expected hash, the original address is tried, unless the chain
    /// has no fallback. A mismatching document from a rewriter that
    /// [trusts its metadata](coblet_runtime::UrlRewriter::trusts_metadata)
    /// is used when nothing else worked, with an [`UNVERIFIED_MIRROR`]
    /// warning, and kept for later offline use.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when the network fails and nothing usable is
    /// cached, when the document does not match the expected hash, or when
    /// the cache cannot be written.
    pub async fn fetch(
        &self,
        request: &MetaRequest,
        policy: CachePolicy,
    ) -> Result<Fetched, CatalogError> {
        let path = self.path_of(&request.key);
        let route = self.rewriters.route(&request.url, &request.kind);
        let trust = route.trust_metadata
            && matches!(request.kind, ArtifactKind::Metadata | ArtifactKind::Loader);
        let sidecar = self
            .read_optional(&self.validators_path(&request.key))
            .await?
            .and_then(|bytes| serde_json::from_slice::<Validators>(&bytes).ok());
        let mut unverified = None;
        let cached = match (self.read_optional(&path).await?, &request.expect) {
            (Some(body), Some(expect)) => match check(&body, expect) {
                Ok(()) => Some(body),
                Err(VerifyError::Hash { actual, .. })
                    if trust
                        && sidecar.as_ref().and_then(|v| v.accepted.as_ref())
                            == Some(&Accepted {
                                expected: expect.to_string(),
                                actual: actual.to_string(),
                            }) =>
                {
                    unverified = Some(unverified_warning(&route.url, expect, &actual));
                    Some(body)
                }
                Err(_) => None,
            },
            (body, _) => body,
        };
        if let Some(body) = &cached
            && (request.expect.is_some() || policy == CachePolicy::PreferCache)
        {
            return Ok(Fetched {
                body: body.clone(),
                origin: Origin::Cache,
                warning: unverified,
            });
        }

        let validators = match (&cached, policy) {
            (Some(_), CachePolicy::Revalidate) => sidecar.as_ref(),
            _ => None,
        };
        let first = match self
            .attempt(request, &route.url, &path, cached.as_ref(), validators)
            .await
        {
            Ok(fetched) => return Ok(fetched),
            Err(failed) => failed,
        };
        let mut failure = first.error;
        if let Some(original) = &route.fallback {
            tracing::info!(mirror = %route.url, %original, error = %failure, "trying the original address");
            match self
                .attempt(request, original, &path, cached.as_ref(), validators)
                .await
            {
                Ok(fetched) => return Ok(fetched),
                Err(failed) => failure = failed.error,
            }
        }
        if trust && let Some(mismatch) = first.mismatch {
            let warning = unverified_warning(&route.url, &mismatch.expected, &mismatch.actual);
            tracing::warn!(url = %route.url, "{}", warning.message);
            let Mismatch {
                body,
                validators,
                expected,
                actual,
            } = *mismatch;
            let validators = Validators {
                accepted: Some(Accepted {
                    expected: expected.to_string(),
                    actual: actual.to_string(),
                }),
                ..validators
            };
            self.store(&request.key, &path, body.clone(), &validators)
                .await?;
            return Ok(Fetched {
                body,
                origin: Origin::Network,
                warning: Some(warning),
            });
        }

        let Some(body) = cached else {
            return Err(failure);
        };
        tracing::warn!(url = %route.url, error = %failure, "using cached metadata");
        Ok(Fetched {
            body,
            origin: Origin::OfflineCache,
            warning: Some(Warning {
                op: None,
                code: OFFLINE_CACHE,
                message: format!("using the cached copy of {}: {failure}", request.url),
            }),
        })
    }

    /// Requests the document from one address.
    async fn attempt(
        &self,
        request: &MetaRequest,
        url: &HttpUrl,
        path: &Path,
        cached: Option<&Bytes>,
        validators: Option<&Validators>,
    ) -> Result<Fetched, Failed> {
        let validators = validators.filter(|validators| validators.url == url.as_str());
        let mut http = Request::get(url.clone());
        if let Some(validators) = validators {
            if let Some(etag) = &validators.etag {
                http = http.header("If-None-Match", etag.clone());
            }
            if let Some(date) = &validators.last_modified {
                http = http.header("If-Modified-Since", date.clone());
            }
        }
        let transport = |source| CatalogError::Transport {
            url: url.clone(),
            source,
        };
        let response = self.transport.send(http).await.map_err(transport)?;
        match response.status() {
            304 if validators.is_some() => {
                return cached
                    .map(|body| Fetched {
                        body: body.clone(),
                        origin: Origin::NotModified,
                        warning: None,
                    })
                    .ok_or_else(|| {
                        CatalogError::Status {
                            url: url.clone(),
                            status: 304,
                        }
                        .into()
                    });
            }
            200 => {}
            status => {
                return Err(CatalogError::Status {
                    url: url.clone(),
                    status,
                }
                .into());
            }
        }
        let fresh = Validators {
            url: url.as_str().to_owned(),
            etag: response.header("ETag").map(str::to_owned),
            last_modified: response.header("Last-Modified").map(str::to_owned),
            accepted: None,
        };
        let body = response.bytes(request.max_size).await.map_err(transport)?;
        if let Some(expect) = &request.expect
            && let Err(source) = check(&body, expect)
        {
            let mismatch = match &source {
                VerifyError::Hash { actual, .. } => Some(Box::new(Mismatch {
                    body,
                    validators: fresh,
                    expected: expect.clone(),
                    actual: (**actual).clone(),
                })),
                _ => None,
            };
            return Err(Failed {
                error: CatalogError::Verify {
                    url: url.clone(),
                    source,
                },
                mismatch,
            });
        }
        self.store(&request.key, path, body.clone(), &fresh).await?;
        Ok(Fetched {
            body,
            origin: Origin::Network,
            warning: None,
        })
    }

    async fn store(
        &self,
        key: &RelPath,
        path: &Path,
        body: Bytes,
        validators: &Validators,
    ) -> Result<(), CatalogError> {
        if let Some(parent) = path.parent() {
            self.storage.create_dir_all(parent).await?;
        }
        self.storage.write_atomic(path, body).await?;
        let sidecar = self.validators_path(key);
        if validators.is_empty() {
            if let Err(error) = self.storage.remove_file(&sidecar).await
                && !error.is_not_found()
            {
                return Err(error.into());
            }
        } else {
            let json = serde_json::to_vec(validators).unwrap_or_default();
            self.storage
                .write_atomic(&sidecar, Bytes::from(json))
                .await?;
        }
        Ok(())
    }
}

fn unverified_warning(url: &HttpUrl, expected: &FileHash, actual: &FileHash) -> Warning {
    Warning {
        op: None,
        code: UNVERIFIED_MIRROR,
        message: format!(
            "using {url} from a trusted mirror although it does not match {expected} (got {actual})"
        ),
    }
}

#[cfg(test)]
#[path = "meta_tests.rs"]
mod tests;
