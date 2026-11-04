use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use coblet_runtime::{
    Expectation, HttpUrl, RelPath, Request, Storage, StreamHasher, Transport, VerifyError, Warning,
    WarningCode,
};
use coblet_types::FileHash;
use serde::{Deserialize, Serialize};

use crate::CatalogError;

/// Warning code published when cached metadata is used because the network failed.
pub const OFFLINE_CACHE: WarningCode = WarningCode::from_static("catalog.offline-cache");

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
        }
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
}

/// HTTP cache for metadata documents, usually rooted at `data_root/cache/meta`.
///
/// Each document is stored at its key with a `<key>.http.json` file holding
/// the `ETag` and `Last-Modified` validators. Version manifests, version
/// JSON files and Mod Loader indexes all go through it, so the launcher can
/// list versions without a network connection.
#[derive(Clone)]
pub struct MetaCache {
    transport: Arc<dyn Transport>,
    storage: Arc<dyn Storage>,
    root: PathBuf,
}

impl std::fmt::Debug for MetaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaCache")
            .field("root", &self.root)
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
        }
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
        let cached = self.read_optional(&path).await?.filter(|body| {
            request
                .expect
                .as_ref()
                .is_none_or(|hash| check(body, hash).is_ok())
        });
        let cache_hit = |body: Bytes, origin| Fetched {
            body,
            origin,
            warning: None,
        };
        if let Some(body) = &cached
            && (request.expect.is_some() || policy == CachePolicy::PreferCache)
        {
            return Ok(cache_hit(body.clone(), Origin::Cache));
        }

        let validators = match (&cached, policy) {
            (Some(_), CachePolicy::Revalidate) => self
                .read_optional(&self.validators_path(&request.key))
                .await?
                .and_then(|bytes| serde_json::from_slice::<Validators>(&bytes).ok())
                .filter(|validators| validators.url == request.url.as_str()),
            _ => None,
        };
        let mut http = Request::get(request.url.clone());
        if let Some(validators) = &validators {
            if let Some(etag) = &validators.etag {
                http = http.header("If-None-Match", etag.clone());
            }
            if let Some(date) = &validators.last_modified {
                http = http.header("If-Modified-Since", date.clone());
            }
        }

        let failure = match self.transport.send(http).await {
            Ok(response) if response.status() == 304 && validators.is_some() => {
                if let Some(body) = cached {
                    return Ok(cache_hit(body, Origin::NotModified));
                }
                CatalogError::Status {
                    url: request.url.clone(),
                    status: 304,
                }
            }
            Ok(response) if response.status() == 200 => {
                let fresh = Validators {
                    url: request.url.as_str().to_owned(),
                    etag: response.header("ETag").map(str::to_owned),
                    last_modified: response.header("Last-Modified").map(str::to_owned),
                };
                match response.bytes(request.max_size).await {
                    Ok(body) => return self.store(request, &path, body, &fresh).await,
                    Err(source) => CatalogError::Transport {
                        url: request.url.clone(),
                        source,
                    },
                }
            }
            Ok(response) => CatalogError::Status {
                url: request.url.clone(),
                status: response.status(),
            },
            Err(source) => CatalogError::Transport {
                url: request.url.clone(),
                source,
            },
        };

        let Some(body) = cached else {
            return Err(failure);
        };
        tracing::warn!(url = %request.url, error = %failure, "using cached metadata");
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

    async fn store(
        &self,
        request: &MetaRequest,
        path: &Path,
        body: Bytes,
        validators: &Validators,
    ) -> Result<Fetched, CatalogError> {
        if let Some(expect) = &request.expect
            && let Err(source) = check(&body, expect)
        {
            return Err(CatalogError::Verify {
                url: request.url.clone(),
                source,
            });
        }
        if let Some(parent) = path.parent() {
            self.storage.create_dir_all(parent).await?;
        }
        self.storage.write_atomic(path, body.clone()).await?;
        let sidecar = self.validators_path(&request.key);
        if validators.etag.is_some() || validators.last_modified.is_some() {
            let json = serde_json::to_vec(validators).unwrap_or_default();
            self.storage
                .write_atomic(&sidecar, Bytes::from(json))
                .await?;
        } else if let Err(error) = self.storage.remove_file(&sidecar).await
            && !error.is_not_found()
        {
            return Err(error.into());
        }
        Ok(Fetched {
            body,
            origin: Origin::Network,
            warning: None,
        })
    }
}

#[cfg(test)]
#[path = "meta_tests.rs"]
mod tests;
