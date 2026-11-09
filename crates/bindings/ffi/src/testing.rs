//! Launch fixtures shared by the binding tests.

use std::path::Path;
use std::sync::Arc;

use coblet::catalog::{CachePolicy, CatalogContext, MetaCache};
use coblet::download::{Downloader, DownloaderConfig};
use coblet::launch::{GameLayout, LaunchContext, LaunchRequest, LaunchServices};
use coblet::mojang::{RawVersionJson, TraitTable};
use coblet::platform::{EffectiveLaunchProfile, ProfileEngine, ResolvedProfile};
use coblet::runtime::{
    BoxFuture, Request, Response, TokioSpawner, TokioStorage, Transport, TransportError,
};
use coblet::types::{Arch, OsFamily, Platform};

/// Transport for launches that need no network; it is never answered.
struct Offline;

impl Transport for Offline {
    fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(std::future::pending())
    }
}

/// Resolves a fixture version for Linux x86-64.
pub(crate) fn profile(version: &str) -> EffectiveLaunchProfile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../fixtures/mojang/{version}.json"));
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
    ProfileEngine::builtin()
        .resolve(&resolved, &Platform::new(OsFamily::Linux, Arch::X86_64))
        .unwrap()
}

/// Creates an offline launch of `version` with data in `root`.
pub(crate) fn launch_context(root: &Path, version: &str, request: LaunchRequest) -> LaunchContext {
    let storage = Arc::new(TokioStorage::new());
    let transport: Arc<dyn Transport> = Arc::new(Offline);
    let meta = MetaCache::new(Arc::clone(&transport), storage.clone(), root.join("meta"));
    let services = LaunchServices::new(
        storage.clone(),
        Arc::new(TokioSpawner::new()),
        Downloader::new(transport, storage, DownloaderConfig::default()),
        Arc::new(CatalogContext::new(meta, CachePolicy::PreferCache)),
        OsFamily::Linux,
    );
    LaunchContext::new(
        services,
        profile(version),
        GameLayout::new(root.join("data"), root.join("game")),
        request,
    )
}
