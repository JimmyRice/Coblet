use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use coblet_catalog::{CatalogContext, CatalogError, MetaRequest, Timestamp};
use coblet_runtime::{ArtifactKind, BoxFuture, Extension, HttpUrl, RelPath};
use coblet_types::{ExtensionId, GameVersionId, VersionType};

use super::{VersionTrait, VersionTraits};
use crate::{Document, GameVersion, RawVersionJson};

/// Extension that adds or removes version traits, for example from another
/// launcher's metadata.
///
/// Trait sources run in the order of their [`TraitSources`](super::TraitSources)
/// list, the built-in table first. Each call receives a whole batch: every
/// listed version when a catalog is loaded, or the one version being
/// launched, then with its version JSON. A source that fails leaves the
/// batch as it found it and the others still run.
///
/// # Examples
///
/// ```
/// use coblet_mojang::{TraitContext, TraitError, TraitSource, TraitTarget, VersionTrait};
/// use coblet_runtime::{BoxFuture, Extension};
/// use coblet_types::ExtensionId;
///
/// struct Snapshots(ExtensionId);
///
/// impl Extension for Snapshots {
///     fn id(&self) -> &ExtensionId { &self.0 }
///     fn display_name(&self) -> &str { "Snapshot marker" }
/// }
///
/// impl TraitSource for Snapshots {
///     fn adjust<'a>(&'a self, _: &'a TraitContext<'a>, versions: &'a mut [TraitTarget])
///         -> BoxFuture<'a, Result<(), TraitError>> {
///         Box::pin(async move {
///             for target in versions.iter_mut() {
///                 if target.input().id.as_str().contains('w') {
///                     target.traits_mut().insert(VersionTrait::from("acme:snapshot"));
///                 }
///             }
///             Ok(())
///         })
///     }
/// }
/// ```
pub trait TraitSource: Extension {
    /// Changes the traits of a batch of versions.
    fn adjust<'a>(
        &'a self,
        cx: &'a TraitContext<'a>,
        versions: &'a mut [TraitTarget],
    ) -> BoxFuture<'a, Result<(), TraitError>>;
}

/// What a [`TraitSource`] knows about one version.
///
/// While a catalog is listed only the facts of the version list are
/// known; when a version is launched, [`version`](Self::version) holds its
/// version JSON as well.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct TraitInput {
    /// Version ID.
    pub id: GameVersionId,
    /// Release channel, when known.
    pub version_type: Option<VersionType>,
    /// Release time, when known.
    pub released: Option<Timestamp>,
    /// Version source that listed the version, such as `coblet:mojang`.
    pub source: Option<ExtensionId>,
    /// The version JSON, present when a version is launched.
    pub version: Option<Arc<Document<RawVersionJson>>>,
}

impl TraitInput {
    /// Creates the input of a version known only by its ID.
    #[must_use]
    pub fn new(id: GameVersionId) -> Self {
        Self {
            id,
            version_type: None,
            released: None,
            source: None,
            version: None,
        }
    }

    /// Creates the input of a listed version, without its version JSON.
    #[must_use]
    pub fn listed(version: &GameVersion) -> Self {
        Self {
            id: version.id.clone(),
            version_type: Some(version.version_type.clone()),
            released: Some(version.released),
            source: version.source.clone(),
            version: None,
        }
    }

    /// Adds the version JSON; its release channel and time fill the ones
    /// not known yet.
    #[must_use]
    pub fn with_version(mut self, document: Arc<Document<RawVersionJson>>) -> Self {
        if self.version_type.is_none() {
            self.version_type.clone_from(&document.value.version_type);
        }
        if self.released.is_none() {
            self.released = document.value.release_time;
        }
        self.version = Some(document);
        self
    }

    /// Returns the parsed version JSON, when present.
    #[must_use]
    pub fn version_json(&self) -> Option<&RawVersionJson> {
        self.version.as_deref().map(|document| &document.value)
    }
}

/// One version in a batch handed to a [`TraitSource`]: what is known about
/// it and its traits so far.
#[derive(Clone, Debug)]
pub struct TraitTarget {
    input: TraitInput,
    traits: VersionTraits,
    changes: Vec<TraitChange>,
}

impl TraitTarget {
    /// Creates a target that starts with `traits`.
    #[must_use]
    pub fn new(input: TraitInput, traits: VersionTraits) -> Self {
        Self {
            input,
            traits,
            changes: Vec::new(),
        }
    }

    /// Returns what is known about the version.
    #[must_use]
    pub fn input(&self) -> &TraitInput {
        &self.input
    }

    /// Returns the traits so far.
    #[must_use]
    pub fn traits(&self) -> &VersionTraits {
        &self.traits
    }

    /// Returns the traits for changing.
    pub fn traits_mut(&mut self) -> &mut VersionTraits {
        &mut self.traits
    }

    /// Returns the changes the trait sources made, in order.
    #[must_use]
    pub fn changes(&self) -> &[TraitChange] {
        &self.changes
    }

    /// Returns the traits and the changes that led to them.
    #[must_use]
    pub fn into_report(self) -> TraitReport {
        TraitReport {
            traits: self.traits,
            changes: self.changes,
        }
    }

    pub(super) fn record(&mut self, source: &ExtensionId, before: &VersionTraits) {
        for value in before.iter().filter(|value| !self.traits.contains(value)) {
            self.changes.push(TraitChange {
                source: source.clone(),
                action: TraitAction::Removed,
                value: value.clone(),
            });
        }
        for value in self.traits.iter().filter(|value| !before.contains(value)) {
            self.changes.push(TraitChange {
                source: source.clone(),
                action: TraitAction::Added,
                value: value.clone(),
            });
        }
    }
}

/// Traits of one version and which trait source added or removed each.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraitReport {
    /// The resulting traits.
    pub traits: VersionTraits,
    /// Every change, in the order the trait sources made them.
    pub changes: Vec<TraitChange>,
}

/// One trait a source added or removed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitChange {
    /// The trait source.
    pub source: ExtensionId,
    /// Whether the trait was added or removed.
    pub action: TraitAction,
    /// The trait.
    pub value: VersionTrait,
}

impl fmt::Display for TraitChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} by {}", self.action, self.value, self.source)
    }
}

/// What a [`TraitChange`] did.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TraitAction {
    /// The trait was added.
    Added,
    /// The trait was removed.
    Removed,
}

impl TraitAction {
    /// Returns `added` or `removed`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
        }
    }
}

impl fmt::Display for TraitAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a [`TraitSource`] may use: cached metadata requests on behalf of
/// one source.
///
/// Requests go through the metadata cache, so mirror rules, fallback to
/// the original address and offline caching apply to third-party sources
/// as to built-in ones. Cache keys are kept apart per source under
/// `traits/<namespace>/<name>/`.
#[derive(Debug)]
pub struct TraitContext<'a> {
    catalog: &'a CatalogContext,
    source: &'a ExtensionId,
}

impl<'a> TraitContext<'a> {
    /// Creates a context for one source.
    #[must_use]
    pub fn new(catalog: &'a CatalogContext, source: &'a ExtensionId) -> Self {
        Self { catalog, source }
    }

    /// Returns the trait source the requests are made for.
    #[must_use]
    pub fn source(&self) -> &ExtensionId {
        self.source
    }

    /// Returns the catalog context, for requests that need more control,
    /// such as an expected hash.
    #[must_use]
    pub fn catalog(&self) -> &CatalogContext {
        self.catalog
    }

    /// Returns the cache key of a document of this source.
    ///
    /// # Errors
    ///
    /// Returns [`TraitError::Failed`] when `name` is not a safe relative path.
    pub fn cache_key(&self, name: &str) -> Result<RelPath, TraitError> {
        RelPath::new(format!(
            "traits/{}/{}/{name}",
            self.source.namespace(),
            self.source.name()
        ))
        .map_err(|error| TraitError::failed(format!("cache key {name}: {error}")))
    }

    /// Fetches a document, cached under [`cache_key`](Self::cache_key)`(name)`.
    ///
    /// # Errors
    ///
    /// Returns [`TraitError::Failed`] for a malformed address or key and
    /// [`TraitError::Metadata`] when the document is unavailable.
    pub async fn fetch(&self, url: &str, name: &str) -> Result<Bytes, TraitError> {
        let address =
            HttpUrl::new(url).map_err(|error| TraitError::failed(format!("{url}: {error}")))?;
        let request =
            MetaRequest::new(address, self.cache_key(name)?).with_kind(ArtifactKind::Metadata);
        Ok(self.catalog.fetch(&request).await?)
    }

    /// Fetches and parses a JSON document, like [`fetch`](Self::fetch).
    ///
    /// # Errors
    ///
    /// Returns the errors of [`fetch`](Self::fetch), and
    /// [`TraitError::Failed`] when the document does not parse.
    pub async fn fetch_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        name: &str,
    ) -> Result<T, TraitError> {
        let body = self.fetch(url, name).await?;
        serde_json::from_slice(&body).map_err(|error| TraitError::failed(format!("{url}: {error}")))
    }
}

/// Failure of a trait source or a trait table.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TraitError {
    /// The source failed.
    #[error("{0}")]
    Failed(String),
    /// A document the source fetched is unavailable.
    #[error(transparent)]
    Metadata(#[from] CatalogError),
    /// A trait table is malformed.
    #[error("invalid trait table: {0}")]
    Table(String),
}

impl TraitError {
    /// Creates a [`TraitError::Failed`].
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed(reason.into())
    }
}

/// A trait source that failed for a batch; its changes were dropped.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
#[error("trait source `{source_id}` failed: {error}")]
pub struct TraitFailure {
    /// The trait source.
    pub source_id: ExtensionId,
    /// What went wrong.
    #[source]
    pub error: TraitError,
}
