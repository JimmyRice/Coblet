use super::CobletBuilder;
use crate::{CobletError, ErrorKind, PlatformInfo};

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
source = { system = "/usr/lib/lwjgl3" }

[java]
source = "system-only"
"#;

fn builder() -> std::sync::Arc<CobletBuilder> {
    CobletBuilder::new(
        "/tmp/coblet-uniffi-profiles".to_owned(),
        "coblet-uniffi-test/1.0".to_owned(),
    )
    .unwrap()
}

fn kind(error: &CobletError) -> ErrorKind {
    let CobletError::Failed { kind, .. } = error;
    *kind
}

#[test]
fn profiles_from_toml_reach_the_coblet() {
    let builder = builder();
    builder.add_platform_profile_toml(LOONGARCH).unwrap();
    let coblet = builder.build().unwrap();
    assert!(
        coblet
            .inner
            .profile_engine()
            .profiles()
            .iter()
            .any(
                |profile| coblet::runtime::Extension::id(profile.as_ref()).as_str()
                    == "community:linux-loongarch64"
            )
    );
    assert_eq!(coblet.platform().os, std::env::consts::OS);

    let error = builder.build().err().unwrap();
    assert_eq!(kind(&error), ErrorKind::InvalidState);
    let error = builder.add_platform_profile_toml(LOONGARCH).unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidState);
}

#[test]
fn bad_profiles_are_rejected_when_added() {
    let builder = builder();
    let error = builder
        .add_platform_profile_toml("id = \"community:x\"\nunknown = 1\n")
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    builder.add_platform_profile_toml(LOONGARCH).unwrap();
    let error = builder.add_platform_profile_toml(LOONGARCH).unwrap_err();
    assert!(error.to_string().contains("already registered"), "{error}");
    let builtin = LOONGARCH.replace("community:linux-loongarch64", "coblet:linux-arm64");
    assert!(builder.add_platform_profile_toml(&builtin).is_err());
}

#[test]
fn the_platform_can_be_described() {
    let builder = builder();
    builder
        .set_platform(PlatformInfo {
            os: "linux".to_owned(),
            arch: "loongarch64".to_owned(),
            os_version: Some("6.6".to_owned()),
            libc: Some("glibc".to_owned()),
            traits: vec!["acme-gpu".to_owned()],
        })
        .unwrap();
    let info = builder.build().unwrap().platform();
    assert_eq!(info.arch, "loongarch64");
    assert_eq!(info.os_version.as_deref(), Some("6.6"));
    assert_eq!(info.traits, ["acme-gpu"]);

    let error = CobletBuilder::new("/tmp/x".to_owned(), "coblet-test/1.0".to_owned())
        .unwrap()
        .set_platform(PlatformInfo {
            os: "Not An OS".to_owned(),
            arch: "x86_64".to_owned(),
            os_version: None,
            libc: None,
            traits: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidArgument);
}

/// Customizer that changes nothing, to name in grants.
struct Login;

impl crate::LaunchCustomizer for Login {
    fn id(&self) -> String {
        "acme:fix".to_owned()
    }

    fn display_name(&self) -> String {
        "Login".to_owned()
    }

    fn applies_to(&self, _traits: Vec<String>) -> bool {
        true
    }

    fn customize(&self, _editor: std::sync::Arc<crate::ProfileEditor>) -> Result<(), CobletError> {
        Ok(())
    }
}

#[test]
fn auth_grants_name_added_customizers_and_reach_the_launcher() {
    let id = coblet::types::ExtensionId::new("acme:fix").unwrap();
    for grant in [false, true] {
        let builder = builder();
        let error = builder.allow_auth_changes("acme:fix").unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        assert_eq!(
            error.to_string(),
            "customizer `acme:fix` was not added; add the customizer first"
        );
        builder.add_customizer(std::sync::Arc::new(Login)).unwrap();
        let error = builder.allow_auth_changes("No Namespace").unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        if grant {
            builder.allow_auth_changes("acme:fix").unwrap();
            builder.allow_auth_changes("acme:fix").unwrap();
        }
        let coblet = builder.build().unwrap();
        assert_eq!(coblet.inner.launcher().allows_auth_changes(&id), grant);
        let error = builder.allow_auth_changes("acme:fix").unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidState);
    }
}

struct Proxy;

impl crate::UrlRewriter for Proxy {
    fn rewrite(&self, url: String, kind: crate::ArtifactKind) -> Option<String> {
        if kind == crate::ArtifactKind::Asset {
            return Some("not a url".to_owned());
        }
        url.strip_prefix("https://mirror.example/")
            .map(|rest| format!("https://proxy.example/{rest}"))
    }
}

#[test]
fn mirrors_and_rewriters_reach_downloads_and_metadata() {
    use coblet::runtime::{ArtifactKind, HttpUrl};

    let builder = builder();
    builder
        .add_mirror(
            "https://libraries.minecraft.net/".to_owned(),
            "https://mirror.example/libraries/".to_owned(),
            vec![crate::ArtifactKind::Library],
            true,
        )
        .unwrap();
    builder.set_mirror_fallback(false).unwrap();
    builder
        .add_url_rewriter(std::sync::Arc::new(Proxy))
        .unwrap();
    let error = builder
        .add_mirror(
            "file:///x".to_owned(),
            "https://a.example/".to_owned(),
            Vec::new(),
            false,
        )
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    let coblet = builder.build().unwrap();

    let library = HttpUrl::new("https://libraries.minecraft.net/a/b.jar").unwrap();
    for rewriters in [
        coblet.inner.meta().rewriters(),
        coblet.inner.downloader().rewriters(),
    ] {
        assert!(!rewriters.falls_back());
        assert!(
            rewriters
                .route(&library, &ArtifactKind::Library)
                .trust_metadata
        );
        assert_eq!(
            rewriters.apply(&library, &ArtifactKind::Library).as_str(),
            "https://proxy.example/libraries/a/b.jar"
        );
        assert_eq!(
            rewriters.apply(&library, &ArtifactKind::Metadata).as_str(),
            library.as_str()
        );
    }
    let asset = HttpUrl::new("https://resources.download.minecraft.net/ab/abc").unwrap();
    assert_eq!(
        coblet
            .inner
            .downloader()
            .rewriters()
            .apply(&asset, &ArtifactKind::Asset)
            .as_str(),
        asset.as_str()
    );
}

#[test]
fn version_manifests_need_unique_ids() {
    let builder = builder();
    let url = "https://meta.example/experiments.json".to_owned();
    builder
        .add_version_manifest("acme:experiments", url.clone())
        .unwrap();
    for id in ["acme:experiments", "coblet:mojang", "no namespace"] {
        let error = builder.add_version_manifest(id, url.clone()).unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument, "{id}");
    }
    let error = builder
        .add_version_manifest("acme:other", "ftp://x".to_owned())
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    builder.build().unwrap();
}
