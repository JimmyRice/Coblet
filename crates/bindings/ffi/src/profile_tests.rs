#![allow(unsafe_code, reason = "drives the C ABI")]

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use coblet::mojang::{RawVersionJson, TraitTable};
use coblet::platform::{
    EffectiveLaunchProfile, JavaSource, ProfileEngine, ProfileError, ResolvedProfile,
};
use coblet::types::{Arch, ArchName, OsFamily, Platform};

use super::{
    CobletJavaSource, CobletNatives, CobletPlatformProfile, CobletProfileDraft, CobletRule,
    CobletRules, ForeignProfile, coblet_draft_add_native_path, coblet_draft_append_arg,
    coblet_draft_fail, coblet_draft_handle_natives, coblet_draft_info_json,
    coblet_draft_set_java_source, coblet_rules_set,
};
use crate::builder::{
    coblet_builder_add_platform_profile, coblet_builder_free, coblet_builder_new,
};
use crate::customize::CobletArgKind;
use crate::types::{CobletStatus, CobletString};

/// State behind `user_data`, as a C program would keep it.
#[derive(Default)]
struct State {
    fail: bool,
    frees: AtomicUsize,
}

fn state(user_data: *mut c_void) -> &'static State {
    // SAFETY: every test passes a pointer to a `State` that outlives the profile.
    unsafe { &*user_data.cast::<State>() }
}

fn json(ptr: *const c_char) -> serde_json::Value {
    // SAFETY: the library passes a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
    serde_json::from_str(text).unwrap()
}

unsafe extern "C" fn matches(_user_data: *mut c_void, target_json: *const c_char) -> bool {
    let target = json(target_json);
    target["platform"]["arch"] == "loongarch64"
        && target["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|component| component["uid"] == "org.lwjgl3")
}

unsafe extern "C" fn rules(
    _user_data: *mut c_void,
    _target: *const c_char,
    rules: *mut CobletRules,
) {
    let linux = CString::new("linux").unwrap();
    // SAFETY: the handle of this call and a valid string.
    let status = unsafe {
        coblet_rules_set(
            rules,
            CobletRule::MojangOs,
            linux.as_ptr(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, CobletStatus::Ok);
}

unsafe extern "C" fn rewrite(user_data: *mut c_void, draft: *mut CobletProfileDraft) -> bool {
    let null = std::ptr::null_mut();
    let text = |value: &str| CString::new(value).unwrap();
    // SAFETY: `draft` is the handle of this call; strings live for each call.
    unsafe {
        if state(user_data).fail {
            let message = text("no GPU driver found");
            coblet_draft_fail(draft, message.as_ptr());
            return false;
        }
        let relative = text("lib");
        assert_eq!(
            coblet_draft_add_native_path(draft, relative.as_ptr(), null),
            CobletStatus::InvalidArgument
        );
        let mut out = CobletString::empty();
        assert_eq!(
            coblet_draft_info_json(draft, &raw mut out, null),
            CobletStatus::Ok
        );
        let info = json(out.ptr);
        crate::context::coblet_string_free(out);
        let system = text("/usr/lib/lwjgl3");
        for library in info["libraries"].as_array().unwrap() {
            if library["has_natives"] != true {
                continue;
            }
            let key = library["native_key"].as_str().unwrap();
            let natives = if key.ends_with(":natives-linux") {
                CobletNatives::System
            } else {
                CobletNatives::NotNeeded
            };
            let pattern = text(key);
            let status = coblet_draft_handle_natives(
                draft,
                pattern.as_ptr(),
                natives,
                system.as_ptr(),
                std::ptr::null(),
                null,
            );
            assert_eq!(status, CobletStatus::Ok);
        }
        let arg = text("-Dorg.lwjgl.librarypath=/usr/lib/lwjgl3");
        assert_eq!(
            coblet_draft_append_arg(draft, CobletArgKind::Jvm, arg.as_ptr(), null),
            CobletStatus::Ok
        );
        assert_eq!(
            coblet_draft_set_java_source(draft, CobletJavaSource::SystemOnly, null),
            CobletStatus::Ok
        );
    }
    true
}

unsafe extern "C" fn free(user_data: *mut c_void) {
    state(user_data).frees.fetch_add(1, Ordering::SeqCst);
}

fn table(state: &State, id: &CStr) -> CobletPlatformProfile {
    CobletPlatformProfile {
        id: id.as_ptr(),
        display_name: std::ptr::null(),
        priority: 100,
        user_data: std::ptr::from_ref(state).cast_mut().cast(),
        matches: Some(matches),
        rules: Some(rules),
        rewrite: Some(rewrite),
        free: Some(free),
    }
}

fn resolved() -> ResolvedProfile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/mojang/1.20.1.json");
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    ResolvedProfile::from_version(raw, traits).unwrap()
}

fn resolve(state: &State) -> Result<EffectiveLaunchProfile, ProfileError> {
    let id = CString::new("acme:loongarch").unwrap();
    // SAFETY: the table is valid and `state` outlives the engine.
    let profile = unsafe { ForeignProfile::new(&table(state, &id)) }.unwrap();
    let mut engine = ProfileEngine::builtin();
    engine.register(std::sync::Arc::new(profile)).unwrap();
    let loongarch = Platform::new(
        OsFamily::Linux,
        Arch::from(ArchName::new("loongarch64").unwrap()),
    );
    engine.resolve(&resolved(), &loongarch)
}

#[test]
fn a_c_profile_adapts_a_new_architecture() {
    let state = State::default();
    let profile = resolve(&state).unwrap();
    assert_eq!(profile.java.source, JavaSource::SystemOnly);
    assert_eq!(
        profile.native_paths,
        [std::path::PathBuf::from("/usr/lib/lwjgl3")]
    );
    let explain = profile.explain();
    assert!(
        explain.contains("[evaluate] acme:loongarch: rules evaluate as os `linux`"),
        "{explain}"
    );
    assert!(explain.contains("natives from system path `/usr/lib/lwjgl3`"));
    assert!(explain.contains(
        "[rewrite] acme:loongarch: jvm `-Dorg.lwjgl.librarypath=/usr/lib/lwjgl3` appended"
    ));
    assert_eq!(state.frees.load(Ordering::SeqCst), 1);
}

#[test]
fn a_false_rewrite_stops_the_launch() {
    let state = State {
        fail: true,
        ..State::default()
    };
    let error = resolve(&state).unwrap_err();
    assert_eq!(
        error.to_string(),
        "platform profile acme:loongarch failed: no GPU driver found"
    );
}

#[test]
fn tables_are_freed_once_even_when_rejected() {
    let state = State::default();
    let good = CString::new("acme:loongarch").unwrap();
    let root = CString::new("/tmp/coblet-ffi-profiles").unwrap();
    let agent = CString::new("coblet-ffi-test/1.0").unwrap();
    let mut builder = std::ptr::null_mut();
    let mut err = std::ptr::null_mut();
    // SAFETY: valid strings, tables and out-pointers; the builder is freed once.
    unsafe {
        let status = coblet_builder_new(
            root.as_ptr(),
            agent.as_ptr(),
            &raw mut builder,
            &raw mut err,
        );
        assert_eq!(status, CobletStatus::Ok);
        let status =
            coblet_builder_add_platform_profile(builder, table(&state, &good), &raw mut err);
        assert_eq!(status, CobletStatus::Ok);
        let status =
            coblet_builder_add_platform_profile(builder, table(&state, &good), &raw mut err);
        assert_eq!(status, CobletStatus::InvalidArgument);
        let message = CStr::from_ptr((*err).message.ptr).to_str().unwrap();
        assert!(message.contains("already registered"), "{message}");
        crate::context::coblet_error_free(err);
        let mut no_matches = table(&state, &good);
        no_matches.matches = None;
        let status = coblet_builder_add_platform_profile(builder, no_matches, std::ptr::null_mut());
        assert_eq!(status, CobletStatus::InvalidArgument);
        assert_eq!(state.frees.load(Ordering::SeqCst), 2);
        coblet_builder_free(builder);
    }
    assert_eq!(state.frees.load(Ordering::SeqCst), 3);
}
