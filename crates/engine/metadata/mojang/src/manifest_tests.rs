use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use coblet_catalog::{CachePolicy, Catalog, CatalogContext, CatalogError, Filter, MetaCache};
use coblet_runtime::{
    BoxFuture, MemoryBody, Request, Response, TokioStorage, Transport, TransportError,
};
use coblet_types::VersionType;

use super::{MojangVersions, VersionManifest, fetch_asset_index, fetch_version_json};
use crate::version::tests::fixture_bytes;
use crate::{AssetIndex, VersionTrait};

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
    let manifest = VersionManifest::from_slice(&fixture_bytes("version_manifest_v2.json")).unwrap();
    assert_eq!(manifest.latest.release.as_str(), "26.3");
    assert_eq!(manifest.versions[0].id.as_str(), "26.4-snapshot-7");
    assert_eq!(manifest.versions[0].compliance_level, 1);
    assert!(manifest.versions.windows(2).all(
        |pair| pair[0].release_time >= pair[1].release_time || pair[1].id.as_str() == "13w16a"
    ));
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
    let catalog = Catalog::new(meta).with_source(Arc::new(MojangVersions::new()));
    let all = catalog.list(&Filter::new()).await.unwrap();
    assert_eq!(all.len(), 14);
    assert_eq!(catalog.recommended(&all).unwrap().id.as_str(), "26.3");
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
        source.entry(entry, &latest)
    };

    let beta = fetch_version_json(&cx, &entry("b1.7.3")).await.unwrap();
    assert_eq!(beta.value.id.as_str(), "b1.7.3");
    assert_eq!(&beta.bytes[..], &fixture_bytes("b1.7.3.json")[..]);
    let traits = source.traits_of(&beta.value);
    assert!(traits.contains(&VersionTrait::JreLegacy));

    // Cached by hash: no second request.
    let before = files.requested.lock().unwrap().len();
    fetch_version_json(&cx, &entry("b1.7.3")).await.unwrap();
    assert_eq!(files.requested.lock().unwrap().len(), before);

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
