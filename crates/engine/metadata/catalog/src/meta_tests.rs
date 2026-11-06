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

#[tokio::test(start_paused = true)]
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
    // One success, then three tries for each failing fetch.
    assert_eq!(transport.count(), 7);
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

#[tokio::test]
async fn rewriters_change_the_address_but_not_the_cache_key() {
    let (dir, transport, cache, request) = setup(vec![
        Reply::Ok(200, vec![("ETag", "\"v1\"")], "abc"),
        Reply::Ok(304, Vec::new(), ""),
        Reply::Ok(200, Vec::new(), "abc"),
    ]);
    let mirror = Arc::new(
        coblet_runtime::PrefixRewriter::new("https://meta.example/", "https://mirror.example/m/")
            .unwrap()
            .for_kinds([coblet_runtime::ArtifactKind::Metadata]),
    );
    let mirrored = cache.clone().with_rewriter(mirror);
    assert_eq!(mirrored.rewriters().len(), 1);

    mirrored
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("cache/meta/example/manifest.json")).unwrap(),
        b"abc"
    );
    let second = mirrored
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(second.origin, Origin::NotModified);
    assert_eq!(
        transport.header_of(1, "If-None-Match").as_deref(),
        Some("\"v1\"")
    );

    // The validators came from the mirror, so the official address is asked
    // without them.
    cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(transport.header_of(2, "If-None-Match"), None);
    let urls: Vec<String> = transport
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.url().as_str().to_owned())
        .collect();
    assert_eq!(
        urls,
        [
            "https://mirror.example/m/manifest.json",
            "https://mirror.example/m/manifest.json",
            "https://meta.example/manifest.json",
        ]
    );
}

#[tokio::test]
async fn rewriters_skip_other_kinds() {
    let (_dir, transport, cache, request) = setup(vec![Reply::Ok(200, Vec::new(), "abc")]);
    let cache = cache.with_rewriter(Arc::new(
        coblet_runtime::PrefixRewriter::new("https://meta.example/", "https://mirror.example/")
            .unwrap()
            .for_kinds([coblet_runtime::ArtifactKind::Metadata]),
    ));
    cache
        .fetch(
            &request.with_kind(coblet_runtime::ArtifactKind::Loader),
            CachePolicy::Revalidate,
        )
        .await
        .unwrap();
    assert_eq!(
        transport.requests.lock().unwrap()[0].url().as_str(),
        "https://meta.example/manifest.json"
    );
}

fn hashed(request: MetaRequest) -> MetaRequest {
    request.with_hash(FileHash::new("sha1", ABC_SHA1).unwrap())
}

fn mirror(trust: bool) -> Arc<coblet_runtime::PrefixRewriter> {
    let rule =
        coblet_runtime::PrefixRewriter::new("https://meta.example/", "https://mirror.example/")
            .unwrap();
    Arc::new(if trust {
        rule.trusting_metadata()
    } else {
        rule
    })
}

fn urls(transport: &Scripted) -> Vec<String> {
    transport
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.url().as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn a_mismatching_mirror_falls_back_to_the_original_address() {
    let (_dir, transport, cache, request) = setup(vec![
        Reply::Ok(200, Vec::new(), "abd"),
        Reply::Ok(200, Vec::new(), "abc"),
    ]);
    let cache = cache.with_rewriter(mirror(true));
    let fetched = cache
        .fetch(&hashed(request), CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(&fetched.body[..], b"abc");
    assert_eq!(fetched.warning, None);
    assert_eq!(
        urls(&transport),
        [
            "https://mirror.example/manifest.json",
            "https://meta.example/manifest.json"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_trusted_mirror_copy_is_used_when_the_original_fails_and_kept() {
    let (_dir, transport, cache, request) =
        setup(vec![Reply::Ok(200, Vec::new(), "abd"), Reply::Fail]);
    let cache = cache.with_rewriter(mirror(true));
    let request = hashed(request);
    let fetched = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(&fetched.body[..], b"abd");
    let warning = fetched.warning.unwrap();
    assert_eq!(warning.code, super::UNVERIFIED_MIRROR);
    assert!(
        warning.message.contains("mirror.example"),
        "{}",
        warning.message
    );

    // The accepted copy serves later fetches without the network, still with a warning.
    let again = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(
        (again.origin, &again.body[..]),
        (Origin::Cache, &b"abd"[..])
    );
    assert_eq!(again.warning.unwrap().code, super::UNVERIFIED_MIRROR);
    // The mirror once, the original three times.
    assert_eq!(transport.count(), 4);
}

#[tokio::test(start_paused = true)]
async fn an_untrusted_mirror_copy_is_rejected() {
    let (dir, transport, cache, request) =
        setup(vec![Reply::Ok(200, Vec::new(), "abd"), Reply::Fail]);
    let cache = cache.with_rewriter(mirror(false));
    let error = cache
        .fetch(&hashed(request), CachePolicy::Revalidate)
        .await
        .unwrap_err();
    assert!(matches!(error, CatalogError::Transport { .. }), "{error}");
    assert_eq!(transport.count(), 4);
    assert!(!dir.path().join("cache/meta/example/manifest.json").exists());
}

#[tokio::test]
async fn without_fallback_only_the_mirror_is_asked() {
    let (_dir, transport, cache, request) = setup(vec![Reply::Ok(200, Vec::new(), "abd")]);
    let cache = cache.with_rewriters(
        coblet_runtime::UrlRewriters::new()
            .with(mirror(true))
            .without_fallback(),
    );
    let fetched = cache
        .fetch(&hashed(request.clone()), CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(&fetched.body[..], b"abd");
    assert_eq!(urls(&transport), ["https://mirror.example/manifest.json"]);

    // Other kinds than metadata are never accepted unverified.
    let (_dir, _transport, cache, request) = setup(vec![Reply::Ok(200, Vec::new(), "abd")]);
    let cache = cache.with_rewriters(
        coblet_runtime::UrlRewriters::new()
            .with(mirror(true))
            .without_fallback(),
    );
    let error = cache
        .fetch(
            &hashed(request).with_kind(coblet_runtime::ArtifactKind::Library),
            CachePolicy::Revalidate,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, CatalogError::Verify { .. }), "{error}");
}

#[tokio::test(start_paused = true)]
async fn transient_failures_are_retried_on_the_same_address() {
    let (_dir, transport, cache, request) = setup(vec![
        Reply::Fail,
        Reply::Ok(503, Vec::new(), ""),
        Reply::Ok(200, Vec::new(), "abc"),
    ]);
    let fetched = cache
        .fetch(&request, CachePolicy::Revalidate)
        .await
        .unwrap();
    assert_eq!(&fetched.body[..], b"abc");
    assert_eq!(transport.count(), 3);

    let (_dir, transport, cache, request) = setup(vec![Reply::Ok(404, Vec::new(), "")]);
    assert!(
        cache
            .fetch(&request, CachePolicy::Revalidate)
            .await
            .is_err()
    );
    assert_eq!(transport.count(), 1, "a 404 is not retried");
}
