use std::path::PathBuf;
use std::sync::Arc;

use coblet_runtime::{
    BoxFuture, MemoryBody, Request, Response, Transport, TransportError, UserAgent,
};
use coblet_types::{Arch, ExtensionId, OsFamily, Platform};

use crate::{Sdk, SdkError};

/// Serves the Mojang fixtures by file name.
struct Fixtures;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../fixtures/mojang")
}

impl Transport for Fixtures {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let url = request.url().as_str().to_owned();
        Box::pin(async move {
            let name = url.rsplit('/').next().unwrap_or_default();
            Ok(match std::fs::read(fixtures().join(name)) {
                Ok(bytes) => Response::new(200, Vec::new(), MemoryBody::new(bytes)),
                Err(_) => Response::new(404, Vec::new(), MemoryBody::new(Vec::new())),
            })
        })
    }
}

fn sdk(root: &std::path::Path) -> Sdk {
    Sdk::builder()
        .data_root(root)
        .transport(Arc::new(Fixtures))
        .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
        .build()
        .unwrap()
}

#[test]
fn the_default_transport_needs_a_user_agent() {
    let error = Sdk::builder().data_root("/tmp/x").build().unwrap_err();
    assert!(matches!(error, SdkError::Config(_)), "{error}");
    let sdk = Sdk::builder()
        .data_root("/tmp/x")
        .user_agent(UserAgent::new("coblet-test/1.0").unwrap())
        .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
        .build()
        .unwrap();
    assert_eq!(sdk.data_root(), std::path::Path::new("/tmp/x"));
}

#[tokio::test]
async fn lists_versions_and_resolves_one() {
    let dir = tempfile::tempdir().unwrap();
    let sdk = sdk(dir.path());
    let versions = sdk.versions(&coblet_catalog::Filter::new()).await.unwrap();
    assert!(versions.len() > 5);
    assert_eq!(sdk.latest_release().await.unwrap().id.as_str(), "26.3");
    assert!(matches!(
        sdk.version("0.0").await,
        Err(SdkError::UnknownVersion(_))
    ));

    let version = sdk.version("1.20.1").await.unwrap();
    let profile = sdk.resolve(&version).await.unwrap();
    let ids: Vec<&str> = profile.profiles.iter().map(ExtensionId::as_str).collect();
    assert_eq!(ids, ["coblet:mojang-macos-arm64", "coblet:macos-lwjgl3"]);
    assert_eq!(profile.java.major, 17);
}
