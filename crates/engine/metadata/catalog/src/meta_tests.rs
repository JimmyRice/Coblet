use std::sync::Arc;

use coblet_runtime::{HttpUrl, RelPath, TokioStorage};
use coblet_types::FileHash;

use super::{CachePolicy, MetaCache, MetaRequest, OFFLINE_CACHE, Origin};
use crate::CatalogError;
use crate::testing::{Reply, Scripted};

const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

fn setup(replies: Vec<Reply>) -> (tempfile::TempDir, Arc<Scripted>, MetaCache, MetaRequest) {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(Scripted::new(replies));
    let cache = MetaCache::new(
        transport.clone(),
        Arc::new(TokioStorage::new()),
        dir.path().join("cache/meta"),
    );
    let request = MetaRequest::new(
        HttpUrl::new("https://meta.example/manifest.json").unwrap(),
        RelPath::new("example/manifest.json").unwrap(),
    );
    (dir, transport, cache, request)
}

#[tokio::test]
async fn revalidates_with_etag_and_last_modified() {
    let (dir, transport, cache, request) = setup(vec![
        Reply::Ok(
            200,
            vec![
                ("ETag", "\"v1\""),
                ("Last-Modified", "Wed, 21 Oct 2026 07:28:00 GMT"),
            ],
            "abc",
        ),
        Reply::Ok(304, Vec::new(), ""),
        Reply::Ok(200, vec![("ETag", "\"v2\"")], "abd"),
    ]);
    let first = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(
        (first.origin, &first.body[..]),
        (Origin::Network, &b"abc"[..])
    );
    assert_eq!(
        std::fs::read(dir.path().join("cache/meta/example/manifest.json")).unwrap(),
        b"abc"
    );
    assert_eq!(transport.header_of(0, "If-None-Match"), None);

    let second = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(
        (second.origin, &second.body[..]),
        (Origin::NotModified, &b"abc"[..])
    );
    assert_eq!(
        transport.header_of(1, "If-None-Match").as_deref(),
        Some("\"v1\"")
    );
    assert_eq!(
        transport.header_of(1, "If-Modified-Since").as_deref(),
        Some("Wed, 21 Oct 2026 07:28:00 GMT")
    );

    let third = cache.fetch(&request, CachePolicy::Refresh).await.unwrap();
    assert_eq!(
        (third.origin, &third.body[..]),
        (Origin::Network, &b"abd"[..])
    );
    assert_eq!(transport.header_of(2, "If-None-Match"), None);
}

#[tokio::test]
async fn falls_back_to_the_cache_with_a_warning() {
    let (_dir, transport, cache, request) =
        setup(vec![Reply::Ok(200, Vec::new(), "abc"), Reply::Fail]);
    cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    let offline = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(offline.origin, Origin::OfflineCache);
    assert_eq!(&offline.body[..], b"abc");
    assert_eq!(offline.warning.unwrap().code, OFFLINE_CACHE);

    transport.push(Reply::Ok(503, Vec::new(), ""));
    let unavailable = cache.fetch(&request, CachePolicy::Refresh).await.unwrap();
    assert_eq!(unavailable.origin, Origin::OfflineCache);
    assert_eq!(transport.count(), 3);
}

#[tokio::test]
async fn fails_without_a_cached_copy() {
    let (_dir, _transport, cache, request) = setup(vec![Reply::Ok(404, Vec::new(), "")]);
    let error = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap_err();
    assert!(
        matches!(error, CatalogError::Status { status: 404, .. }),
        "{error}"
    );
    let error = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap_err();
    assert!(matches!(error, CatalogError::Transport { .. }), "{error}");
}

#[tokio::test]
async fn trusts_a_cached_copy_that_matches_the_hash() {
    let (_dir, transport, cache, request) = setup(vec![
        Reply::Ok(200, Vec::new(), "abd"),
        Reply::Ok(200, Vec::new(), "abc"),
    ]);
    let request = request.with_hash(FileHash::new("sha1", ABC_SHA1).unwrap());
    let error = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap_err();
    assert!(matches!(error, CatalogError::Verify { .. }), "{error}");
    let fetched = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(fetched.origin, Origin::Network);
    for policy in [CachePolicy::Revalidate, CachePolicy::Refresh] {
        let cached = cache.fetch(&request, policy).await.unwrap();
        assert_eq!(cached.origin, Origin::Cache);
    }
    assert_eq!(transport.count(), 2);
}

#[tokio::test]
async fn prefer_cache_skips_the_network() {
    let (_dir, transport, cache, request) = setup(vec![Reply::Ok(200, Vec::new(), "abc")]);
    assert_eq!(
        cache
            .fetch(&request, CachePolicy::PreferCache)
            .await
            .unwrap()
            .origin,
        Origin::Network
    );
    assert_eq!(
        cache
            .fetch(&request, CachePolicy::PreferCache)
            .await
            .unwrap()
            .origin,
        Origin::Cache
    );
    assert_eq!(transport.count(), 1);
}
