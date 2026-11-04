use std::sync::Arc;

use bytes::Bytes;
use coblet_catalog::{
    CatalogContext, CatalogError, CatalogItem, CatalogPage, CatalogQuery, CatalogSource,
    MetaRequest, Timestamp,
};
use coblet_runtime::{BoxFuture, Extension, HttpUrl, RelPath};
use coblet_types::{ExtensionId, FileHash, GameVersionId, Sha1Digest, VersionType};
use serde::{Deserialize, Serialize};

use crate::{AssetIndex, AssetIndexRef, RawVersionJson, TraitSource, TraitTable, VersionTraits};

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
    /// Traits known before the version JSON is fetched; see [`TraitTable::for_manifest_entry`].
    pub traits: VersionTraits,
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
    table: Arc<TraitTable>,
    trait_sources: Vec<Arc<dyn TraitSource>>,
}

impl std::fmt::Debug for MojangVersions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MojangVersions")
            .field("manifest", &self.manifest)
            .field("trait_sources", &self.trait_sources.len())
            .finish_non_exhaustive()
    }
}

impl Default for MojangVersions {
    fn default() -> Self {
        Self::new()
    }
}

impl MojangVersions {
    /// Creates the source with the official manifest address and the built-in trait table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: ExtensionId::new("coblet:mojang").unwrap_or_else(|_| unreachable!()),
            manifest: HttpUrl::new(MANIFEST_URL).unwrap_or_else(|_| unreachable!()),
            table: Arc::new(TraitTable::builtin().clone()),
            trait_sources: Vec::new(),
        }
    }

    /// Uses another manifest address, such as a mirror.
    #[must_use]
    pub fn with_manifest_url(self, manifest: HttpUrl) -> Self {
        Self { manifest, ..self }
    }

    /// Uses another trait table.
    #[must_use]
    pub fn with_trait_table(self, table: TraitTable) -> Self {
        Self {
            table: Arc::new(table),
            ..self
        }
    }

    /// Adds a trait source, run after the table.
    #[must_use]
    pub fn with_trait_source(mut self, source: Arc<dyn TraitSource>) -> Self {
        self.trait_sources.push(source);
        self
    }

    /// Returns the traits of a version JSON: the table, then the trait sources.
    #[must_use]
    pub fn traits_of(&self, raw: &RawVersionJson) -> VersionTraits {
        let mut traits = self.table.for_version(raw);
        for source in &self.trait_sources {
            source.adjust(&raw.id, &mut traits);
        }
        traits
    }

    /// Fetches and parses the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when the manifest is unavailable or malformed.
    pub async fn manifest(&self, cx: &CatalogContext) -> Result<VersionManifest, CatalogError> {
        let request = MetaRequest::new(
            self.manifest.clone(),
            key("mojang/version_manifest_v2.json".to_owned())?,
        );
        let bytes = cx.fetch(&request).await?;
        VersionManifest::from_slice(&bytes)
            .map_err(|error| CatalogError::parse(self.manifest.as_str(), error))
    }

    fn entry(&self, entry: ManifestEntry, latest: &GameVersionId) -> GameVersion {
        let mut traits = self
            .table
            .for_manifest_entry(&entry.id, Some(entry.release_time));
        for source in &self.trait_sources {
            source.adjust(&entry.id, &mut traits);
        }
        GameVersion {
            latest_release: entry.id == *latest,
            id: entry.id,
            version_type: entry.version_type,
            url: entry.url,
            sha1: entry.sha1,
            released: entry.release_time,
            compliance_level: entry.compliance_level,
            traits,
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
                .map(|entry| self.entry(entry, &latest))
                .collect();
            Ok(CatalogPage::new(items))
        })
    }
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
#[path = "manifest_tests.rs"]
mod tests;
