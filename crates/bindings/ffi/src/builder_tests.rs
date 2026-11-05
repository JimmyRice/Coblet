#![allow(unsafe_code, reason = "drives the C ABI")]

use std::ffi::{CStr, CString};

use super::{
    coblet_builder_add_platform_profile, coblet_builder_build, coblet_builder_free,
    coblet_builder_new,
};
use crate::context::{coblet_error_free, coblet_free};
use crate::types::CobletStatus;

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

/// Adds `toml` and returns the status with the error message, if any.
fn add(builder: *mut super::CobletBuilder, toml: &str) -> (CobletStatus, Option<String>) {
    let toml = CString::new(toml).unwrap();
    let mut err = std::ptr::null_mut();
    // SAFETY: a live builder, a valid string and out-pointer.
    let status =
        unsafe { coblet_builder_add_platform_profile(builder, toml.as_ptr(), &raw mut err) };
    let message = (!err.is_null()).then(|| {
        // SAFETY: the error was just returned and is freed once.
        unsafe {
            let text = CStr::from_ptr((*err).message.ptr)
                .to_str()
                .unwrap()
                .to_owned();
            coblet_error_free(err);
            text
        }
    });
    (status, message)
}

fn builder() -> *mut super::CobletBuilder {
    let root = CString::new("/tmp/coblet-ffi-profiles").unwrap();
    let agent = CString::new("coblet-ffi-test/1.0").unwrap();
    let mut builder = std::ptr::null_mut();
    // SAFETY: valid strings and out-pointers.
    let status = unsafe {
        coblet_builder_new(
            root.as_ptr(),
            agent.as_ptr(),
            &raw mut builder,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, CobletStatus::Ok);
    builder
}

#[test]
fn profiles_from_toml_reach_the_coblet() {
    let builder = builder();
    assert_eq!(add(builder, LOONGARCH), (CobletStatus::Ok, None));
    let mut coblet = std::ptr::null_mut();
    // SAFETY: the builder is live and consumed here; the handle is freed once.
    unsafe {
        let status = coblet_builder_build(builder, &raw mut coblet, std::ptr::null_mut());
        assert_eq!(status, CobletStatus::Ok);
        let ids: Vec<String> = (*coblet)
            .inner
            .profile_engine()
            .profiles()
            .iter()
            .map(|profile| profile.id().to_string())
            .collect();
        assert!(
            ids.contains(&"community:linux-loongarch64".to_owned()),
            "{ids:?}"
        );
        assert!(ids.contains(&"coblet:linux-arm64".to_owned()), "{ids:?}");
        coblet_free(coblet);
    }
}

#[test]
fn bad_profiles_are_rejected_when_added() {
    let builder = builder();
    let (status, message) = add(builder, "id = \"community:x\"\nunknown = 1\n");
    assert_eq!(status, CobletStatus::InvalidArgument);
    assert!(
        message
            .unwrap()
            .starts_with("toml invalid platform profile")
    );

    assert_eq!(add(builder, LOONGARCH).0, CobletStatus::Ok);
    let (status, message) = add(builder, LOONGARCH);
    assert_eq!(status, CobletStatus::InvalidArgument);
    assert!(message.unwrap().contains("already registered"));

    let builtin = LOONGARCH.replace("community:linux-loongarch64", "coblet:linux-arm64");
    assert_eq!(add(builder, &builtin).0, CobletStatus::InvalidArgument);
    // SAFETY: the builder is live and not used afterwards.
    unsafe { coblet_builder_free(builder) };
}
