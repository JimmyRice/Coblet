use super::CobletBuilder;
use crate::{CobletError, ErrorKind};

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
    builder.add_platform_profile(LOONGARCH).unwrap();
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
    let error = builder.add_platform_profile(LOONGARCH).unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidState);
}

#[test]
fn bad_profiles_are_rejected_when_added() {
    let builder = builder();
    let error = builder
        .add_platform_profile("id = \"community:x\"\nunknown = 1\n")
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    builder.add_platform_profile(LOONGARCH).unwrap();
    let error = builder.add_platform_profile(LOONGARCH).unwrap_err();
    assert!(error.to_string().contains("already registered"), "{error}");
    let builtin = LOONGARCH.replace("community:linux-loongarch64", "coblet:linux-arm64");
    assert!(builder.add_platform_profile(&builtin).is_err());
}
