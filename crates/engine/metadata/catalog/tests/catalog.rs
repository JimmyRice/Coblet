use std::sync::Arc;

use coblet_runtime::{
    BoxFuture, Event, EventBus, Extension, HttpUrl, Received, RelPath, TokioStorage,
};
use coblet_types::{ExtensionId, GameVersionId, VersionType};

use super::{Catalog, SOURCE_FAILED};
use crate::testing::{Reply, Scripted};
use crate::{
    CatalogContext, CatalogError, CatalogItem, CatalogPage, CatalogQuery, CatalogSource, Filter,
    MetaCache, MetaRequest, OFFLINE_CACHE, Timestamp,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    id: String,
    kind: VersionType,
    time: Option<Timestamp>,
    star: bool,
    from: &'static str,
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
            let text = std::str::from_utf8(&body).map_err(|e| CatalogError::parse(self.url, e))?;
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
