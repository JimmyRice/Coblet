use std::sync::{Mutex, PoisonError};

use bytes::Bytes;
use coblet_runtime::{Extension, Warning};

use crate::{CachePolicy, CatalogError, CatalogPage, CatalogQuery, MetaCache, MetaRequest};
use coblet_runtime::BoxFuture;

/// Provider of catalog items, such as the Mojang version manifest or a Mod Loader's Maven index.
///
/// Built-in and third-party sources implement the same trait. A source
/// fetches documents through [`CatalogContext::fetch`] so they are cached and
/// survive a missing network connection.
pub trait CatalogSource<Item>: Extension {
    /// Lists the items for a query, in the source's own order (newest first when it has one).
    fn fetch<'a>(
        &'a self,
        cx: &'a CatalogContext,
        query: &'a CatalogQuery,
    ) -> BoxFuture<'a, Result<CatalogPage<Item>, CatalogError>>;
}

/// What a [`CatalogSource`] can use while fetching: the metadata cache and the cache policy.
#[derive(Debug)]
pub struct CatalogContext {
    meta: MetaCache,
    policy: CachePolicy,
    warnings: Mutex<Vec<Warning>>,
}

impl CatalogContext {
    /// Creates a context.
    #[must_use]
    pub fn new(meta: MetaCache, policy: CachePolicy) -> Self {
        Self {
            meta,
            policy,
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// Returns the metadata cache.
    #[must_use]
    pub fn meta(&self) -> &MetaCache {
        &self.meta
    }

    /// Returns the cache policy of this listing.
    #[must_use]
    pub fn policy(&self) -> CachePolicy {
        self.policy
    }

    /// Fetches a document with the context's policy and records any warning.
    ///
    /// # Errors
    ///
    /// Returns the [`CatalogError`] of [`MetaCache::fetch`].
    pub async fn fetch(&self, request: &MetaRequest) -> Result<Bytes, CatalogError> {
        let fetched = self.meta.fetch(request, self.policy).await?;
        if let Some(warning) = fetched.warning {
            self.warn(warning);
        }
        Ok(fetched.body)
    }

    /// Records a warning; the catalog publishes it after the listing.
    pub fn warn(&self, warning: Warning) {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(warning);
    }

    /// Removes and returns the recorded warnings.
    pub fn take_warnings(&self) -> Vec<Warning> {
        std::mem::take(&mut *self.warnings.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Step that changes the merged list of a [`Catalog`](crate::Catalog)
/// before the catalog keeps it, such as one that tags game versions.
///
/// Stages run after every source has answered, on the deduplicated and
/// sorted list, once per listing. A stage cannot fail the listing: it
/// reports problems with [`CatalogContext::warn`].
pub trait CatalogStage<Item>: Send + Sync {
    /// Changes the items in place.
    fn apply<'a>(&'a self, cx: &'a CatalogContext, items: &'a mut [Item]) -> BoxFuture<'a, ()>;
}
