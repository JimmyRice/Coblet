use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use coblet_runtime::{Event, EventBus, Warning, WarningCode};
use coblet_types::VersionType;

use crate::{
    CachePolicy, CatalogContext, CatalogError, CatalogItem, CatalogQuery, CatalogSource, Filter,
    MetaCache,
};

/// Warning code published when one of several sources fails and the others still answered.
pub const SOURCE_FAILED: WarningCode = WarningCode::from_static("catalog.source-failed");

/// Merged list of items from several [`CatalogSource`]s.
///
/// Sources are asked in the order they were added. When two sources return
/// the same [`CatalogItem::key`], the earlier source wins. The merged list is
/// sorted by release time, newest first, keeping source order for ties and
/// for items without a time; version strings are never compared. Lists are
/// kept in memory per [`CatalogQuery`] until [`Catalog::refresh`].
pub struct Catalog<Item> {
    sources: Vec<Arc<dyn CatalogSource<Item>>>,
    meta: MetaCache,
    events: Option<EventBus>,
    lists: Mutex<HashMap<CatalogQuery, Arc<[Item]>>>,
}

impl<Item: 'static> std::fmt::Debug for Catalog<Item> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field(
                "sources",
                &self
                    .sources
                    .iter()
                    .map(|source| source.id().as_str())
                    .collect::<Vec<_>>(),
            )
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

impl<Item: CatalogItem> Catalog<Item> {
    /// Creates a catalog without sources.
    #[must_use]
    pub fn new(meta: MetaCache) -> Self {
        Self {
            sources: Vec::new(),
            meta,
            events: None,
            lists: Mutex::new(HashMap::new()),
        }
    }

    /// Adds a source after the existing ones.
    #[must_use]
    pub fn with_source(mut self, source: Arc<dyn CatalogSource<Item>>) -> Self {
        self.sources.push(source);
        self
    }

    /// Publishes warnings, such as falling back to cached metadata, on `events`.
    #[must_use]
    pub fn with_events(self, events: EventBus) -> Self {
        Self {
            events: Some(events),
            ..self
        }
    }

    /// Returns the sources in the order they are asked.
    #[must_use]
    pub fn sources(&self) -> &[Arc<dyn CatalogSource<Item>>] {
        &self.sources
    }

    /// Lists the items that match `filter`, newest first.
    ///
    /// The first call for a [`CatalogQuery`] (derived from [`Filter::game`])
    /// asks the sources, revalidating cached documents; later calls reuse
    /// the list until [`Catalog::refresh`].
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when every source fails.
    pub async fn list(&self, filter: &Filter) -> Result<Vec<Item>, CatalogError> {
        let query = CatalogQuery {
            game: filter.game.clone(),
        };
        let cached = self
            .lists
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&query)
            .cloned();
        let items = match cached {
            Some(items) => items,
            None => self.load(&query, CachePolicy::Revalidate).await?,
        };
        Ok(items
            .iter()
            .filter(|item| filter.matches(*item))
            .cloned()
            .collect())
    }

    /// Fetches every list again, bypassing cached documents.
    ///
    /// Refreshes the lists already loaded, or the unfiltered list when none is.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when every source fails for a list; the
    /// previous list then stays in place.
    pub async fn refresh(&self) -> Result<(), CatalogError> {
        let mut queries: Vec<CatalogQuery> = self
            .lists
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        if queries.is_empty() {
            queries.push(CatalogQuery::all());
        }
        for query in &queries {
            self.load(query, CachePolicy::Refresh).await?;
        }
        Ok(())
    }

    /// Returns the item to preselect: the first one a source recommends,
    /// otherwise the newest release, otherwise the first item.
    #[must_use]
    pub fn recommended<'a>(&self, items: &'a [Item]) -> Option<&'a Item> {
        items
            .iter()
            .find(|item| item.is_recommended())
            .or_else(|| {
                items
                    .iter()
                    .find(|item| *item.version_type() == VersionType::Release)
            })
            .or_else(|| items.first())
    }

    fn publish(&self, warning: Warning) {
        if let Some(events) = &self.events {
            events.publish(Event::Warning(warning));
        }
    }

    async fn load(
        &self,
        query: &CatalogQuery,
        policy: CachePolicy,
    ) -> Result<Arc<[Item]>, CatalogError> {
        let mut pages = Vec::new();
        let mut first_error = None;
        let mut failures = Vec::new();
        for source in &self.sources {
            let cx = CatalogContext::new(self.meta.clone(), policy);
            let result = source.fetch(&cx, query).await;
            for warning in cx.take_warnings() {
                self.publish(warning);
            }
            match result {
                Ok(page) => pages.push(page.items),
                Err(error) => {
                    let error = CatalogError::Source {
                        source_id: source.id().clone(),
                        error: Box::new(error),
                    };
                    tracing::warn!(%error, "catalog source failed");
                    failures.push(error.to_string());
                    first_error.get_or_insert(error);
                }
            }
        }
        if pages.is_empty() {
            return Err(first_error.unwrap_or(CatalogError::NoSources));
        }
        for message in failures {
            self.publish(Warning {
                op: None,
                code: SOURCE_FAILED,
                message,
            });
        }

        let mut seen = HashSet::new();
        let mut merged: Vec<Item> = pages
            .into_iter()
            .flatten()
            .filter(|item| seen.insert(item.key().to_owned()))
            .collect();
        merged.sort_by_key(|item| std::cmp::Reverse(item.released()));
        let merged: Arc<[Item]> = merged.into();
        self.lists
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(query.clone(), Arc::clone(&merged));
        Ok(merged)
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
