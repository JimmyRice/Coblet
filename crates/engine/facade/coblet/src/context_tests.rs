use std::path::PathBuf;
use std::sync::Arc;

use coblet_runtime::{
    BoxFuture, MemoryBody, Request, Response, Transport, TransportError, UserAgent,
};
use coblet_types::{Arch, ExtensionId, OsFamily, Platform};

use crate::{Coblet, Error};

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

fn coblet(root: &std::path::Path) -> Coblet {
    Coblet::builder()
        .data_root(root)
        .transport(Arc::new(Fixtures))
        .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
        .build()
        .unwrap()
}

#[test]
fn the_default_transport_needs_a_user_agent() {
    let error = Coblet::builder().data_root("/tmp/x").build().unwrap_err();
    assert!(matches!(error, Error::Config(_)), "{error}");
    let coblet = Coblet::builder()
        .data_root("/tmp/x")
        .user_agent(UserAgent::new("coblet-test/1.0").unwrap())
        .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
        .build()
        .unwrap();
    assert_eq!(coblet.data_root(), std::path::Path::new("/tmp/x"));
    let relative = Coblet::builder()
        .data_root("data")
        .transport(Arc::new(Fixtures))
        .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
        .build()
        .unwrap();
    assert!(relative.data_root().is_absolute());
}

#[tokio::test]
async fn lists_versions_and_resolves_one() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = coblet(dir.path());
    let versions = coblet
        .versions(&coblet_catalog::Filter::new())
        .await
        .unwrap();
    assert!(versions.len() > 5);
    assert_eq!(coblet.latest_release().await.unwrap().id.as_str(), "26.3");
    assert!(matches!(
        coblet.version("0.0").await,
        Err(Error::UnknownVersion(_))
    ));

    let version = coblet.version("1.20.1").await.unwrap();
    let profile = coblet.resolve(&version).await.unwrap();
    let ids: Vec<&str> = profile.profiles.iter().map(ExtensionId::as_str).collect();
    assert_eq!(ids, ["coblet:mojang-macos-arm64", "coblet:macos-lwjgl3"]);
    assert_eq!(profile.java.major, 17);
}

struct Noop(ExtensionId);

impl coblet_runtime::Extension for Noop {
    fn id(&self) -> &ExtensionId {
        &self.0
    }

    fn display_name(&self) -> &'static str {
        "Noop"
    }
}

impl coblet_launch::LaunchCustomizer for Noop {
    fn applies_to(&self, _traits: &coblet_mojang::VersionTraits) -> bool {
        true
    }

    fn customize(
        &self,
        _profile: &mut coblet_platform::EffectiveLaunchProfile,
        _ctx: &coblet_launch::CustomizeCtx,
    ) -> Result<(), coblet_launch::LaunchError> {
        Ok(())
    }
}

#[test]
fn customizers_need_distinct_ids() {
    let id = ExtensionId::new("acme:noop").unwrap();
    let builder = || {
        Coblet::builder()
            .data_root("/tmp/x")
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
            .customizer(Arc::new(Noop(id.clone())))
    };
    let error = builder()
        .customizer(Arc::new(Noop(id.clone())))
        .build()
        .unwrap_err();
    assert!(matches!(error, Error::Registry(_)), "{error}");
    let coblet = builder().build().unwrap();
    assert!(coblet.inner.launcher.customizers().get(&id).is_some());
}

/// Profile a community could publish for an architecture coblet does not name.
const LOONGARCH: &str = r#"
id = "community:linux-loongarch64"

[match]
os = ["linux"]
arch = ["loongarch64"]
requires_component = { uid = "org.lwjgl3" }

[rules]
mojang_os = "linux"

[[natives]]
match = "org.lwjgl:*:*:natives-linux"
source = { maven = "https://maven.example.org/", classifier = "natives-linux-loongarch64" }

[java]
source = "system-only"
"#;

fn loongarch(root: &std::path::Path) -> crate::CobletBuilder<crate::WithRoot> {
    Coblet::builder()
        .data_root(root)
        .transport(Arc::new(Fixtures))
        .platform(Platform::new(
            OsFamily::Linux,
            "loongarch64".parse().unwrap(),
        ))
}

#[tokio::test]
async fn registered_profiles_adapt_unknown_architectures() {
    let dir = tempfile::tempdir().unwrap();
    let plain = loongarch(dir.path()).build().unwrap();
    let version = plain.version("1.20.1").await.unwrap();
    let error = plain.resolve(&version).await.unwrap_err();
    assert!(matches!(error, Error::Profile(_)), "{error}");

    let profile = coblet_platform::DataProfile::from_toml(
        LOONGARCH,
        coblet_platform::ProfileLayer::ThirdParty,
    )
    .unwrap();
    let extended = loongarch(dir.path())
        .platform_profile(Arc::new(profile))
        .build()
        .unwrap();
    let resolved = extended.resolve(&version).await.unwrap();
    let ids: Vec<&str> = resolved.profiles.iter().map(ExtensionId::as_str).collect();
    assert_eq!(ids, ["community:linux-loongarch64"]);
    assert_eq!(
        resolved.java.source,
        coblet_platform::JavaSource::SystemOnly
    );
    let natives: Vec<&str> = resolved
        .classpath()
        .map(|file| file.path.as_str())
        .filter(|path| path.contains("natives-linux"))
        .collect();
    assert!(!natives.is_empty(), "{}", resolved.explain());
    assert!(
        natives
            .iter()
            .all(|path| path.ends_with("-natives-linux-loongarch64.jar")),
        "{natives:?}"
    );
    assert!(
        extended
            .profile_engine()
            .profiles()
            .iter()
            .any(|known| known.id().as_str() == "community:linux-loongarch64")
    );
}

#[test]
fn platform_profiles_need_distinct_ids() {
    let profile = || {
        Arc::new(
            coblet_platform::DataProfile::from_toml(
                LOONGARCH,
                coblet_platform::ProfileLayer::ThirdParty,
            )
            .unwrap(),
        )
    };
    let error = loongarch(std::path::Path::new("/tmp/x"))
        .platform_profile(profile())
        .platform_profile(profile())
        .build()
        .unwrap_err();
    assert!(matches!(error, Error::Profile(_)), "{error}");
}
