use bytes::Bytes;
use coblet_catalog::{CatalogContext, MetaRequest};
use coblet_runtime::{ArtifactKind, HttpUrl, RelPath};
use coblet_types::ExtensionId;

use crate::LoaderError;

/// What a Mod Loader may use while listing or installing: cached metadata
/// requests on behalf of one loader.
///
/// Every request goes through the metadata cache with
/// [`ArtifactKind::Loader`], so mirror rules, fallback to the original
/// address and offline caching apply to third-party loaders exactly as to
/// built-in ones. Cache keys are kept apart per loader under
/// `loaders/<namespace>/<name>/`.
#[derive(Debug)]
pub struct LoaderContext<'a> {
    catalog: &'a CatalogContext,
    loader: &'a ExtensionId,
}

impl<'a> LoaderContext<'a> {
    /// Creates a context for one Mod Loader.
    #[must_use]
    pub fn new(catalog: &'a CatalogContext, loader: &'a ExtensionId) -> Self {
        Self { catalog, loader }
    }

    /// Returns the Mod Loader the requests are made for.
    #[must_use]
    pub fn loader(&self) -> &ExtensionId {
        self.loader
    }

    /// Returns the catalog context, for requests that need more control,
    /// such as an expected hash.
    #[must_use]
    pub fn catalog(&self) -> &CatalogContext {
        self.catalog
    }

    /// Returns the cache key of a document of this loader.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::InvalidMetadata`] when `name` is not a safe relative path.
    pub fn cache_key(&self, name: &str) -> Result<RelPath, LoaderError> {
        RelPath::new(format!(
            "loaders/{}/{}/{name}",
            self.loader.namespace(),
            self.loader.name()
        ))
        .map_err(|error| LoaderError::invalid(self.loader, format!("cache key {name}: {error}")))
    }

    /// Fetches a document, cached under [`cache_key`](Self::cache_key)`(name)`.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::InvalidMetadata`] for a malformed address or key
    /// and [`LoaderError::Catalog`] when the document is unavailable.
    pub async fn fetch(&self, url: &str, name: &str) -> Result<Bytes, LoaderError> {
        let address = HttpUrl::new(url)
            .map_err(|error| LoaderError::invalid(self.loader, format!("{url}: {error}")))?;
        let request =
            MetaRequest::new(address, self.cache_key(name)?).with_kind(ArtifactKind::Loader);
        Ok(self.catalog.fetch(&request).await?)
    }

    /// Fetches and parses a JSON document, like [`fetch`](Self::fetch).
    ///
    /// # Errors
    ///
    /// Returns the errors of [`fetch`](Self::fetch), and
    /// [`LoaderError::InvalidMetadata`] when the document does not parse.
    pub async fn fetch_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        name: &str,
    ) -> Result<T, LoaderError> {
        let body = self.fetch(url, name).await?;
        serde_json::from_slice(&body)
            .map_err(|error| LoaderError::invalid(self.loader, format!("{url}: {error}")))
    }
}
