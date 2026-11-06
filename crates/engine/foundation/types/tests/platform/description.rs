use super::Platform;
use crate::{Arch, Libc, OsFamily, PlatformTrait};

#[test]
fn serializes_only_known_fields() {
    let linux = Platform::new(OsFamily::Linux, Arch::Riscv64).with_libc(Libc::Musl);
    let json = serde_json::to_string(&linux).unwrap();
    assert_eq!(json, r#"{"os":"linux","arch":"riscv64","libc":"musl"}"#);
    assert_eq!(serde_json::from_str::<Platform>(&json).unwrap(), linux);
}

#[test]
fn describes_unknown_platforms() {
    let json = r#"{"os":"haiku","arch":"sparc64","traits":["Wayland"]}"#;
    let platform: Platform = serde_json::from_str(json).unwrap();
    assert_eq!(platform.os().as_str(), "haiku");
    assert_eq!(platform.arch().as_str(), "sparc64");
    assert!(platform.has_trait("WAYLAND"));
    assert!(!platform.has_trait("rosetta"));
    assert!(!platform.has_trait("not a trait"));
    assert_eq!(
        platform.traits().iter().collect::<Vec<_>>(),
        [&PlatformTrait::new("wayland").unwrap()]
    );
}
