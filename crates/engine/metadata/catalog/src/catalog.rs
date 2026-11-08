use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use coblet_runtime::{Event, EventBus, Warning, WarningCode};
use coblet_types::VersionType;

use crate::{
    CachePolicy, CatalogContext, CatalogError, CatalogItem, CatalogQuery, CatalogSource,
    CatalogSources, CatalogStage, Filter, MetaCache,
};

/// Warning code published when one of several sources fails and the others still answered.
pub const SOURCE_FAILED: WarningCode = WarningCode::from_static("catalog.source-failed");

/// Merged list of items from several [`CatalogSource`]s.
///
/// Sources are asked in the order they were added. When two sources return
/// the same [`CatalogItem::key`], the earlier source wins. The merged list is
/// sorted by release time, newest first, keeping source order for ties and
/// for items without a time; version strings are never compared. Then the
/// [`CatalogStage`]s change the list in the order they were added. Lists are
/// kept in memory per [`CatalogQuery`] until [`Catalog::refresh`].
pub struct Catalog<Item> {
    sources: Vec<Arc<dyn CatalogSource<Item>>>,
    stages: Vec<Arc<dyn CatalogStage<Item>>>,
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
            stages: Vec::new(),
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

    /// Adds every source of `sources` after the existing ones, in order.
    #[must_use]
    pub fn with_sources(mut self, sources: &CatalogSources<Item>) -> Self {
        self.sources.extend(sources.iter().cloned());
        self
    }

    /// Adds a stage that changes every merged list, after the existing stages.
    #[must_use]
    pub fn with_stage(mut self, stage: Arc<dyn CatalogStage<Item>>) -> Self {
        self.stages.push(stage);
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
                Ok(mut page) => {
                    for item in &mut page.items {
                        item.listed_by(source.id());
                    }
                    pages.push(page.items);
                }
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
        if !self.stages.is_empty() {
            let cx = CatalogContext::new(self.meta.clone(), policy);
            for stage in &self.stages {
                stage.apply(&cx, &mut merged).await;
            }
            for warning in cx.take_warnings() {
                self.publish(warning);
            }
        }
        let merged: Arc<[Item]> = merged.into();
        self.lists
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(query.clone(), Arc::clone(&merged));
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_runtime::{
        BoxFuture, Event, EventBus, Extension, HttpUrl, Received, RelPath, TokioStorage, Warning,
        WarningCode,
    };
    use coblet_types::{ExtensionId, GameVersionId, VersionType};

    use super::{Catalog, SOURCE_FAILED};
    use crate::CatalogStage;
    use crate::testing::{Reply, Scripted};
    use crate::{
        CatalogContext, CatalogError, CatalogItem, CatalogPage, CatalogQuery, CatalogSource,
        Filter, MetaCache, MetaRequest, OFFLINE_CACHE, Timestamp,
    };

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Version {
        id: String,
        kind: VersionType,
        time: Option<Timestamp>,
        star: bool,
        from: &'static str,
        by: Option<ExtensionId>,
        note: Option<String>,
    }

    impl CatalogItem for Version {
        fn key(&self) -> &str {
            &self.id
        }
        fn version_type(&self) -> &VersionType {
            &self.kind
        }
        fn released(&self) -> Option<Timestamp> {
            self.time
        }
        fn is_recommended(&self) -> bool {
            self.star
        }
        fn is_compatible_with(&self, game: &GameVersionId) -> bool {
            self.id.starts_with(game.as_str())
        }
        fn listed_by(&mut self, source: &ExtensionId) {
            self.by = Some(source.clone());
        }
    }

    /// Source that parses `id,type,time` lines from a document.
    struct Lines {
        id: ExtensionId,
        url: &'static str,
    }

    impl Extension for Lines {
        fn id(&self) -> &ExtensionId {
            &self.id
        }
        fn display_name(&self) -> &'static str {
            "lines"
        }
    }

    impl CatalogSource<Version> for Lines {
        fn fetch<'a>(
            &'a self,
            cx: &'a CatalogContext,
            _query: &'a CatalogQuery,
        ) -> BoxFuture<'a, Result<CatalogPage<Version>, CatalogError>> {
            Box::pin(async move {
                let request = MetaRequest::new(
                    HttpUrl::new(self.url).unwrap(),
                    RelPath::new(format!("{}.txt", self.id.as_str().replace(':', "/"))).unwrap(),
                );
                let body = cx.fetch(&request).await?;
                let text =
                    std::str::from_utf8(&body).map_err(|e| CatalogError::parse(self.url, e))?;
                let from = if self.id.as_str() == "test:main" {
                    "main"
                } else {
                    "extra"
                };
                let items = text
                    .lines()
                    .map(|line| {
                        let mut parts = line.split(',');
                        let id = parts.next().unwrap_or_default().to_owned();
                        let kind = VersionType::from(parts.next().unwrap_or_default());
                        let time = parts.next().and_then(|t| t.parse().ok());
                        Version {
                            star: parts.next() == Some("*"),
                            id,
                            kind,
                            time,
                            from,
                            by: None,
                            note: None,
                        }
                    })
                    .collect();
                Ok(CatalogPage::new(items))
            })
        }
    }

    const MAIN: &str = "26.3,release,2026-09-01T10:00:00Z\n\
26.4-snapshot-1,snapshot,2026-09-20T10:00:00Z\n\
1.20.1,release,2023-06-12T13:25:51+00:00\n\
b1.7.3,old_beta,2011-07-07T22:00:00+00:00";
    const EXTRA: &str = "1.20.1,release,2023-06-12T13:25:51Z\n\
combat-test-8c,experiment,2020-08-14T12:00:00Z\n\
mystery,experiment";

    fn catalog(transport: Arc<Scripted>, dir: &tempfile::TempDir) -> Catalog<Version> {
        let meta = MetaCache::new(transport, Arc::new(TokioStorage::new()), dir.path());
        let source = |id: &str, url| -> Arc<dyn CatalogSource<Version>> {
            Arc::new(Lines {
                id: ExtensionId::new(id).unwrap(),
                url,
            })
        };
        Catalog::new(meta)
            .with_source(source("test:main", "https://meta.example/main"))
            .with_source(source("test:extra", "https://meta.example/extra"))
    }

    fn ids(items: &[Version]) -> Vec<&str> {
        items.iter().map(|item| item.id.as_str()).collect()
    }

    #[tokio::test]
    async fn merges_dedupes_and_sorts_by_release_time() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Scripted::new([
            Reply::Ok(200, Vec::new(), MAIN),
            Reply::Ok(200, Vec::new(), EXTRA),
        ]));
        let catalog = catalog(transport.clone(), &dir);
        let all = catalog.list(&Filter::new()).await.unwrap();
        assert_eq!(
            ids(&all),
            [
                "26.4-snapshot-1",
                "26.3",
                "1.20.1",
                "combat-test-8c",
                "b1.7.3",
                "mystery"
            ]
        );
        let duplicate = all.iter().find(|item| item.id == "1.20.1").unwrap();
        assert_eq!(duplicate.from, "main");
        assert_eq!(catalog.recommended(&all).unwrap().id, "26.3");

        let releases = catalog
            .list(&Filter::new().with_types([VersionType::Release]))
            .await
            .unwrap();
        assert_eq!(ids(&releases), ["26.3", "1.20.1"]);
        let window = Filter::new()
            .released_after("2011-01-01T00:00:00Z".parse().unwrap())
            .released_before("2021-01-01T00:00:00Z".parse().unwrap());
        assert_eq!(
            ids(&catalog.list(&window).await.unwrap()),
            ["combat-test-8c", "b1.7.3"]
        );
        assert_eq!(
            ids(&catalog
                .list(&Filter::new().with_text("SNAP"))
                .await
                .unwrap()),
            ["26.4-snapshot-1"]
        );
        // Listings are served from memory.
        assert_eq!(transport.count(), 2);
    }

    #[tokio::test]
    async fn keeps_working_when_a_source_fails_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Scripted::new([
            Reply::Ok(200, Vec::new(), MAIN),
            Reply::Ok(500, Vec::new(), ""),
        ]));
        let events = EventBus::new(16);
        let mut subscription = events.subscribe();
        let catalog = catalog(transport.clone(), &dir).with_events(events);
        let all = catalog.list(&Filter::new()).await.unwrap();
        assert_eq!(all.len(), 4);
        let Some(Received::Event(Event::Warning(warning))) = subscription.recv().await else {
            panic!("expected a warning");
        };
        assert_eq!(warning.code, SOURCE_FAILED);

        // Offline refresh: the main source falls back to its cache, the extra one has none.
        let error = {
            transport.push(Reply::Fail);
            transport.push(Reply::Fail);
            catalog.refresh().await
        };
        assert!(error.is_ok());
        let Some(Received::Event(Event::Warning(warning))) = subscription.recv().await else {
            panic!("expected a warning");
        };
        assert_eq!(warning.code, OFFLINE_CACHE);
    }

    #[tokio::test]
    async fn fails_when_every_source_fails() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Scripted::new([Reply::Fail, Reply::Fail]));
        let error = catalog(transport, &dir)
            .list(&Filter::new())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, CatalogError::Source { source_id, .. } if source_id.as_str() == "test:main"),
            "{error}"
        );
        let empty = Catalog::<Version>::new(MetaCache::new(
            Arc::new(Scripted::default()),
            Arc::new(TokioStorage::new()),
            "/nowhere",
        ));
        assert!(matches!(
            empty.list(&Filter::new()).await,
            Err(CatalogError::NoSources)
        ));
    }

    #[tokio::test]
    async fn recommends_a_starred_item_and_filters_by_game() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Scripted::new([
            Reply::Ok(
                200,
                Vec::new(),
                "1.20.1-b,release,,*\n1.20.1-a,release\n1.19,release",
            ),
            Reply::Ok(200, Vec::new(), ""),
        ]));
        let catalog = catalog(transport, &dir);
        let game = GameVersionId::new("1.20.1").unwrap();
        let items = catalog
            .list(&Filter::new().compatible_with(game))
            .await
            .unwrap();
        assert_eq!(ids(&items), ["1.20.1-b", "1.20.1-a"]);
        assert_eq!(catalog.recommended(&items).unwrap().id, "1.20.1-b");
        assert!(catalog.recommended(&[]).is_none());
    }

    /// Notes the source of every item and warns once.
    struct Note;

    impl CatalogStage<Version> for Note {
        fn apply<'a>(
            &'a self,
            cx: &'a CatalogContext,
            items: &'a mut [Version],
        ) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                for item in items.iter_mut() {
                    item.note = item.by.as_ref().map(|by| format!("from {by}"));
                }
                cx.warn(Warning {
                    op: None,
                    code: WarningCode::from_static("test.noted"),
                    message: format!("noted {}", items.len()),
                });
            })
        }
    }

    #[tokio::test]
    async fn stages_see_the_merged_list_with_its_sources() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Scripted::new([
            Reply::Ok(200, Vec::new(), MAIN),
            Reply::Ok(200, Vec::new(), EXTRA),
        ]));
        let events = EventBus::new(16);
        let mut subscription = events.subscribe();
        let catalog = catalog(transport, &dir)
            .with_stage(Arc::new(Note))
            .with_events(events);
        let all = catalog.list(&Filter::new()).await.unwrap();
        let note = |id: &str| {
            all.iter()
                .find(|item| item.id == id)
                .and_then(|item| item.note.clone())
        };
        assert_eq!(note("1.20.1").as_deref(), Some("from test:main"));
        assert_eq!(note("mystery").as_deref(), Some("from test:extra"));
        let Some(Received::Event(Event::Warning(warning))) = subscription.recv().await else {
            panic!("expected a warning");
        };
        assert_eq!(warning.message, "noted 6");
        // The list is kept with the stage's changes.
        let again = catalog.list(&Filter::new()).await.unwrap();
        assert!(again.iter().all(|item| item.note.is_some()));
    }
}
