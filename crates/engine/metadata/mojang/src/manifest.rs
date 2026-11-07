use bytes::Bytes;
use coblet_catalog::{
    CatalogContext, CatalogError, CatalogItem, CatalogPage, CatalogQuery, CatalogSource,
    MetaRequest, Timestamp,
};
use coblet_runtime::{BoxFuture, Extension, HttpUrl, RelPath};
use coblet_types::{ExtensionId, FileHash, GameVersionId, Sha1Digest, VersionType};
use serde::{Deserialize, Serialize};

use crate::{AssetIndex, AssetIndexRef, RawVersionJson, VersionTraits};

/// Address of the Mojang version manifest.
pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// Parsed `version_manifest_v2.json`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionManifest {
    /// Newest release and snapshot.
    pub latest: Latest,
    /// Every version, newest first.
    pub versions: Vec<ManifestEntry>,
}

impl VersionManifest {
    /// Parses a manifest.
    ///
    /// # Errors
    ///
    /// Returns the [`serde_json::Error`] of malformed documents.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Newest versions named by the manifest.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Latest {
    /// Newest release.
    pub release: GameVersionId,
    /// Newest snapshot.
    pub snapshot: GameVersionId,
}

/// One version in the manifest.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    /// Version ID.
    pub id: GameVersionId,
    /// Release channel.
    #[serde(rename = "type")]
    pub version_type: VersionType,
    /// Address of the version JSON.
    pub url: HttpUrl,
    /// Last modification of the version JSON.
    pub time: Timestamp,
    /// Release time.
    pub release_time: Timestamp,
    /// SHA-1 of the version JSON.
    pub sha1: Sha1Digest,
    /// Safety level; 1 marks versions the official launcher warns about.
    #[serde(default)]
    pub compliance_level: u32,
}

/// Game version listed by the Mojang catalog source.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameVersion {
    /// Version ID.
    pub id: GameVersionId,
    /// Release channel.
    pub version_type: VersionType,
    /// Address of the version JSON.
    pub url: HttpUrl,
    /// SHA-1 of the version JSON.
    pub sha1: Sha1Digest,
    /// Release time.
    pub released: Timestamp,
    /// Safety level from the manifest.
    pub compliance_level: u32,
    /// Whether the manifest names this version as the newest release.
    pub latest_release: bool,
    /// Traits known before the version JSON is fetched: the declared
    /// traits as changed by the catalog's [`TraitSources`](crate::TraitSources).
    pub traits: VersionTraits,
    /// Traits the source gave the version itself, before any trait source ran.
    pub declared_traits: VersionTraits,
    /// Version source that listed the version, set by the catalog.
    pub source: Option<ExtensionId>,
}

impl GameVersion {
    /// Creates a version for a catalog source other than the Mojang manifest.
    ///
    /// The version is not the latest release, has compliance level 0 and
    /// declares no traits; the trait sources still give it traits.
    ///
    /// # Examples
    ///
    /// ```
    /// use coblet_catalog::Timestamp;
    /// use coblet_mojang::GameVersion;
    /// use coblet_runtime::HttpUrl;
    /// use coblet_types::{GameVersionId, Sha1Digest, VersionType};
    ///
    /// let version = GameVersion::new(
    ///     GameVersionId::new("1.16_combat-6")?,
    ///     VersionType::Experiment,
    ///     HttpUrl::new("https://meta.example/1.16_combat-6.json")?,
    ///     Sha1Digest::from_hex("a9993e364706816aba3e25717850c26c9cd0d89d")?,
    ///     "2020-08-14T12:00:00Z".parse::<Timestamp>()?,
    /// );
    /// assert!(!version.latest_release);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn new(
        id: GameVersionId,
        version_type: VersionType,
        url: HttpUrl,
        sha1: Sha1Digest,
        released: Timestamp,
    ) -> Self {
        Self {
            id,
            version_type,
            url,
            sha1,
            released,
            compliance_level: 0,
            latest_release: false,
            traits: VersionTraits::new(),
            declared_traits: VersionTraits::new(),
            source: None,
        }
    }

    /// Sets the traits the source knows about the version itself; trait
    /// sources start from them.
    #[must_use]
    pub fn with_traits(self, traits: VersionTraits) -> Self {
        Self {
            declared_traits: traits.clone(),
            traits,
            ..self
        }
    }
}

impl CatalogItem for GameVersion {
    fn key(&self) -> &str {
        self.id.as_str()
    }

    fn version_type(&self) -> &VersionType {
        &self.version_type
    }

    fn released(&self) -> Option<Timestamp> {
        Some(self.released)
    }

    fn is_recommended(&self) -> bool {
        self.latest_release
    }

    fn listed_by(&mut self, source: &ExtensionId) {
        self.source = Some(source.clone());
    }
}

/// Parsed document together with the bytes it was parsed from, for writing it into an Instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Document<T> {
    /// The parsed value.
    pub value: T,
    /// The bytes as fetched.
    pub bytes: Bytes,
}

fn key(path: String) -> Result<RelPath, CatalogError> {
    RelPath::new(path.clone()).map_err(|_| CatalogError::InvalidKey(path))
}

fn url(text: &str) -> Result<HttpUrl, CatalogError> {
    HttpUrl::new(text).map_err(|error| CatalogError::parse(text, error))
}

/// Catalog source for the Mojang version manifest, ID `coblet:mojang`.
///
/// Experimental versions such as Combat Tests are not in the manifest; add
/// another source to the catalog for them.
pub struct MojangVersions {
    id: ExtensionId,
    manifest: HttpUrl,
}

impl std::fmt::Debug for MojangVersions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MojangVersions")
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

impl Default for MojangVersions {
    fn default() -> Self {
        Self::new()
    }
}

impl MojangVersions {
    /// Creates the source with the official manifest address.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: ExtensionId::new("coblet:mojang").unwrap_or_else(|_| unreachable!()),
            manifest: HttpUrl::new(MANIFEST_URL).unwrap_or_else(|_| unreachable!()),
        }
    }

    /// Uses another manifest address, such as a mirror.
    #[must_use]
    pub fn with_manifest_url(self, manifest: HttpUrl) -> Self {
        Self { manifest, ..self }
    }

    /// Creates a second source that reads a manifest in the same format
    /// from another address, such as a list of experimental versions.
    ///
    /// The source has its own ID and its own cache entry, so it can sit in
    /// a catalog next to the official one.
    ///
    /// # Examples
    ///
    /// ```
    /// use coblet_mojang::MojangVersions;
    /// use coblet_runtime::{Extension, HttpUrl};
    /// use coblet_types::ExtensionId;
    ///
    /// let experiments = MojangVersions::other(
    ///     ExtensionId::new("acme:experiments")?,
    ///     HttpUrl::new("https://meta.example/experiments.json")?,
    /// );
    /// assert_eq!(experiments.id().as_str(), "acme:experiments");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn other(id: ExtensionId, manifest: HttpUrl) -> Self {
        Self { id, manifest }
    }

    /// Fetches and parses the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when the manifest is unavailable or malformed.
    pub async fn manifest(&self, cx: &CatalogContext) -> Result<VersionManifest, CatalogError> {
        let path = if self.id.as_str() == "coblet:mojang" {
            "mojang/version_manifest_v2.json".to_owned()
        } else {
            format!(
                "sources/{}/manifest.json",
                self.id.as_str().replace(':', "/")
            )
        };
        let request = MetaRequest::new(self.manifest.clone(), key(path)?);
        let bytes = cx.fetch(&request).await?;
        VersionManifest::from_slice(&bytes)
            .map_err(|error| CatalogError::parse(self.manifest.as_str(), error))
    }

    fn entry(entry: ManifestEntry, latest: &GameVersionId) -> GameVersion {
        GameVersion {
            latest_release: entry.id == *latest,
            id: entry.id,
            version_type: entry.version_type,
            url: entry.url,
            sha1: entry.sha1,
            released: entry.release_time,
            compliance_level: entry.compliance_level,
            traits: VersionTraits::new(),
            declared_traits: VersionTraits::new(),
            source: None,
        }
    }
}

impl Extension for MojangVersions {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &'static str {
        "Mojang"
    }
}

impl CatalogSource<GameVersion> for MojangVersions {
    fn fetch<'a>(
        &'a self,
        cx: &'a CatalogContext,
        _query: &'a CatalogQuery,
    ) -> BoxFuture<'a, Result<CatalogPage<GameVersion>, CatalogError>> {
        Box::pin(async move {
            let manifest = self.manifest(cx).await?;
            let latest = manifest.latest.release;
            let items = manifest
                .versions
                .into_iter()
                .map(|entry| Self::entry(entry, &latest))
                .collect();
            Ok(CatalogPage::new(items))
        })
    }
}

/// Fetches a document for a game version source written as code, such as
/// its own version list, cached under
/// `sources/<namespace>/<name>/<file>` next to the manifests of
/// [`MojangVersions::other`].
///
/// The request goes through the context's metadata cache, so mirror rules,
/// fallback to the original address and offline caching apply.
///
/// # Errors
///
/// Returns [`CatalogError`] for a malformed address or file name, and when
/// the document is unavailable and not cached.
pub async fn fetch_for_source(
    cx: &CatalogContext,
    source: &ExtensionId,
    address: &str,
    file: &str,
) -> Result<Bytes, CatalogError> {
    let path = format!("sources/{}/{}/{file}", source.namespace(), source.name());
    cx.fetch(&MetaRequest::new(url(address)?, key(path)?)).await
}

/// Fetches the version JSON of a listed version, checked against the manifest's SHA-1.
///
/// The document is cached under `mojang/versions/<sha1>.json`, so later
/// calls do not touch the network.
///
/// # Errors
///
/// Returns [`CatalogError`] when the document is unavailable, does not
/// match the hash or is malformed.
pub async fn fetch_version_json(
    cx: &CatalogContext,
    version: &GameVersion,
) -> Result<Document<RawVersionJson>, CatalogError> {
    let request = MetaRequest::new(
        version.url.clone(),
        key(format!("mojang/versions/{}.json", version.sha1))?,
    )
    .with_hash(FileHash::Sha1(version.sha1));
    let bytes = cx.fetch(&request).await?;
    let value = RawVersionJson::from_slice(&bytes)
        .map_err(|error| CatalogError::parse(version.url.as_str(), error))?;
    Ok(Document { value, bytes })
}

/// Fetches the asset index a version JSON refers to, checked against its SHA-1.
///
/// # Errors
///
/// Returns [`CatalogError`] when the index is unavailable, does not match
/// the hash or is malformed.
pub async fn fetch_asset_index(
    cx: &CatalogContext,
    index: &AssetIndexRef,
) -> Result<Document<AssetIndex>, CatalogError> {
    let request = MetaRequest::new(
        url(&index.url)?,
        key(format!("mojang/assets/{}.json", index.sha1))?,
    )
    .with_hash(FileHash::Sha1(index.sha1));
    let bytes = cx.fetch(&request).await?;
    let value =
        AssetIndex::from_slice(&bytes).map_err(|error| CatalogError::parse(&index.url, error))?;
    Ok(Document { value, bytes })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use coblet_catalog::{CachePolicy, Catalog, CatalogContext, CatalogError, Filter, MetaCache};
    use coblet_runtime::{
        BoxFuture, MemoryBody, Request, Response, TokioStorage, Transport, TransportError,
    };
    use coblet_types::{ExtensionId, VersionType};

    use super::{
        MojangVersions, VersionManifest, fetch_asset_index, fetch_for_source, fetch_version_json,
    };
    use crate::version::tests::fixture_bytes;
    use crate::{AssetIndex, TraitSources, TraitTable, VersionTrait};

    /// Serves fixture files by URL suffix and records requested URLs.
    #[derive(Default)]
    struct Files {
        routes: Vec<(&'static str, Vec<u8>)>,
        requested: Mutex<VecDeque<String>>,
    }

    impl Transport for Files {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            self.requested.lock().unwrap().push_back(url.clone());
            let body = self
                .routes
                .iter()
                .find(|(suffix, _)| url.ends_with(suffix))
                .map(|(_, body)| body.clone());
            Box::pin(async move {
                Ok(match body {
                    Some(body) => Response::new(200, Vec::new(), MemoryBody::new(body)),
                    None => Response::new(404, Vec::new(), MemoryBody::new("")),
                })
            })
        }
    }

    fn meta(files: Files, dir: &tempfile::TempDir) -> (Arc<Files>, MetaCache) {
        let files = Arc::new(files);
        let meta = MetaCache::new(files.clone(), Arc::new(TokioStorage::new()), dir.path());
        (files, meta)
    }

    #[test]
    fn parses_the_manifest_fixture() {
        let manifest =
            VersionManifest::from_slice(&fixture_bytes("version_manifest_v2.json")).unwrap();
        assert_eq!(manifest.latest.release.as_str(), "26.3");
        assert_eq!(manifest.versions[0].id.as_str(), "26.4-snapshot-7");
        assert_eq!(manifest.versions[0].compliance_level, 1);
        assert!(
            manifest
                .versions
                .windows(2)
                .all(|pair| pair[0].release_time >= pair[1].release_time
                    || pair[1].id.as_str() == "13w16a")
        );
    }

    #[tokio::test]
    async fn lists_versions_and_recommends_the_latest_release() {
        let dir = tempfile::tempdir().unwrap();
        let (_files, meta) = meta(
            Files {
                routes: vec![(
                    "version_manifest_v2.json",
                    fixture_bytes("version_manifest_v2.json"),
                )],
                ..Files::default()
            },
            &dir,
        );
        let catalog = Catalog::new(meta)
            .with_source(Arc::new(MojangVersions::new()))
            .with_stage(Arc::new(TraitSources::builtin()));
        let all = catalog.list(&Filter::new()).await.unwrap();
        assert_eq!(all.len(), 14);
        assert_eq!(catalog.recommended(&all).unwrap().id.as_str(), "26.3");
        assert!(all.iter().all(|version| {
            version.source.as_ref().map(ExtensionId::as_str) == Some("coblet:mojang")
                && version.declared_traits.is_empty()
        }));
        let betas = catalog
            .list(&Filter::new().with_types([VersionType::OldBeta]))
            .await
            .unwrap();
        assert_eq!(betas.len(), 1);
        assert!(
            betas[0]
                .traits
                .contains(&VersionTrait::LegacyOnlineServices)
        );
        // 1.5.2 was released after 13w16a; the catalog orders by release time.
        let ids: Vec<&str> = all.iter().map(|version| version.id.as_str()).collect();
        let position = |id| ids.iter().position(|x| *x == id).unwrap();
        assert!(position("1.5.2") < position("13w16a"));
    }

    #[tokio::test]
    async fn fetches_and_verifies_version_json_and_asset_index() {
        let dir = tempfile::tempdir().unwrap();
        let (files, meta) = meta(
            Files {
                routes: vec![
                    (
                        "version_manifest_v2.json",
                        fixture_bytes("version_manifest_v2.json"),
                    ),
                    ("/b1.7.3.json", fixture_bytes("b1.7.3.json")),
                    ("/1.12.2.json", b"{\"id\":\"tampered\"}".to_vec()),
                    ("/pre-1.6.json", fixture_bytes("assets/pre-1.6.json")),
                ],
                ..Files::default()
            },
            &dir,
        );
        let cx = CatalogContext::new(meta, CachePolicy::Revalidate);
        let source = MojangVersions::new();
        let manifest = source.manifest(&cx).await.unwrap();
        let entry = |id: &str| {
            let latest = manifest.latest.release.clone();
            let entry = manifest
                .versions
                .iter()
                .find(|entry| entry.id.as_str() == id)
                .unwrap()
                .clone();
            MojangVersions::entry(entry, &latest)
        };

        let beta = fetch_version_json(&cx, &entry("b1.7.3")).await.unwrap();
        assert_eq!(beta.value.id.as_str(), "b1.7.3");
        assert_eq!(&beta.bytes[..], &fixture_bytes("b1.7.3.json")[..]);
        let traits = TraitTable::builtin().for_version(&beta.value);
        assert!(traits.contains(&VersionTrait::JreLegacy));

        // Cached by hash: no second request.
        let before = files.requested.lock().unwrap().len();
        fetch_version_json(&cx, &entry("b1.7.3")).await.unwrap();
        assert_eq!(files.requested.lock().unwrap().len(), before);

        let source = coblet_types::ExtensionId::new("acme:copy").unwrap();
        let address = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
        let copied = fetch_for_source(&cx, &source, address, "list.json")
            .await
            .unwrap();
        assert!(!copied.is_empty());
        assert!(dir.path().join("sources/acme/copy/list.json").is_file());
        assert!(
            fetch_for_source(&cx, &source, address, "../escape.json")
                .await
                .is_err()
        );

        let tampered = fetch_version_json(&cx, &entry("1.12.2")).await.unwrap_err();
        assert!(
            matches!(tampered, CatalogError::Verify { .. }),
            "{tampered}"
        );

        // The trimmed fixture does not match the real index hash.
        let index = beta.value.asset_index.as_ref().unwrap();
        let error = fetch_asset_index(&cx, index).await.unwrap_err();
        assert!(matches!(error, CatalogError::Verify { .. }), "{error}");
        let parsed = AssetIndex::from_slice(&fixture_bytes("assets/pre-1.6.json")).unwrap();
        assert!(parsed.map_to_resources);
        assert_eq!(parsed.objects.len(), 4);
        let modern = AssetIndex::from_slice(&fixture_bytes("assets/1.12.json")).unwrap();
        assert!(!modern.map_to_resources && !modern.is_virtual);
        assert!(modern.total_size() > 0);
    }

    #[tokio::test]
    async fn another_manifest_source_has_its_own_cache_entry() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = fixture_bytes("version_manifest_v2.json");
        let (_files, meta) = meta(
            Files {
                routes: vec![
                    ("version_manifest_v2.json", manifest.clone()),
                    ("experiments.json", manifest),
                ],
                ..Files::default()
            },
            &dir,
        );
        let cx = CatalogContext::new(meta, CachePolicy::Revalidate);
        let official = MojangVersions::new();
        let other = MojangVersions::other(
            coblet_types::ExtensionId::new("acme:experiments").unwrap(),
            coblet_runtime::HttpUrl::new("https://meta.example/experiments.json").unwrap(),
        );
        official.manifest(&cx).await.unwrap();
        other.manifest(&cx).await.unwrap();
        assert!(dir.path().join("mojang/version_manifest_v2.json").is_file());
        assert!(
            dir.path()
                .join("sources/acme/experiments/manifest.json")
                .is_file()
        );
    }
}
