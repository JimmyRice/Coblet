#![allow(unsafe_code, reason = "drives the C ABI")]

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use coblet::catalog::{CachePolicy, CatalogContext, MetaCache};
use coblet::download::{Downloader, DownloaderConfig};
use coblet::launch::{
    CustomizeCtx, GameLayout, LaunchContext, LaunchError, LaunchRequest, LaunchServices, Launcher,
};
use coblet::launch_command::LaunchAuth;
use coblet::mojang::{RawVersionJson, TraitTable};
use coblet::platform::{EffectiveLaunchProfile, ProfileEngine, ResolvedProfile};
use coblet::runtime::{
    BoxFuture, Request, Response, TokioSpawner, TokioStorage, Transport, TransportError,
};
use coblet::types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

use super::{
    CobletArgKind, CobletCustomizer, CobletProfileEdit, CobletStep, CobletStepFailure,
    ForeignCustomizer, coblet_edit_add_arg, coblet_edit_add_library, coblet_edit_add_step,
    coblet_edit_fail, coblet_edit_info_json, coblet_edit_remove_library, coblet_edit_set_env,
    coblet_edit_set_main_class, coblet_step_fail,
};
use crate::builder::{coblet_builder_add_customizer, coblet_builder_free, coblet_builder_new};
use crate::types::{CobletStatus, CobletString};

/// State behind `user_data`, as a C program would keep it.
#[derive(Default)]
struct State {
    fail: bool,
    frees: AtomicUsize,
    step_runs: AtomicUsize,
    step_frees: AtomicUsize,
    last_info: std::sync::Mutex<String>,
}

fn state(user_data: *mut c_void) -> &'static State {
    // SAFETY: every test passes a pointer to a `State` that outlives the launch.
    unsafe { &*user_data.cast::<State>() }
}

unsafe extern "C" fn applies_to(_user_data: *mut c_void, traits_json: *const c_char) -> bool {
    // SAFETY: the library passes a NUL-terminated string.
    let traits = unsafe { CStr::from_ptr(traits_json) }.to_str().unwrap();
    traits.contains("\"applet\"")
}

unsafe extern "C" fn run_step(
    user_data: *mut c_void,
    info_json: *const c_char,
    failure: *mut CobletStepFailure,
) -> bool {
    let state = state(user_data);
    state.step_runs.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the library passes a NUL-terminated string.
    let info = unsafe { CStr::from_ptr(info_json) }.to_str().unwrap();
    info.clone_into(&mut state.last_info.lock().unwrap());
    if state.fail {
        let message = CString::new("proxy did not start").unwrap();
        // SAFETY: the failure handle of this call.
        unsafe { coblet_step_fail(failure, message.as_ptr()) };
        return false;
    }
    true
}

unsafe extern "C" fn free_step(user_data: *mut c_void) {
    state(user_data).step_frees.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn customize(user_data: *mut c_void, edit: *mut CobletProfileEdit) -> bool {
    let state = state(user_data);
    let null = std::ptr::null_mut();
    let text = |value: &str| CString::new(value).unwrap();
    // SAFETY: `edit` is the handle of this call; strings live for each call.
    unsafe {
        if state.fail {
            let message = text("agent jar is missing");
            coblet_edit_fail(edit, message.as_ptr());
            return false;
        }
        let mut json = CobletString::empty();
        assert_eq!(
            coblet_edit_info_json(edit, &raw mut json, null),
            CobletStatus::Ok
        );
        let info = CStr::from_ptr(json.ptr).to_str().unwrap().to_owned();
        crate::context::coblet_string_free(json);
        assert!(
            info.contains("\"main_class\":\"net.minecraft.launchwrapper.Launch\""),
            "{info}"
        );
        assert!(info.contains("\"os\":\"linux\""), "{info}");

        let main = text("acme.Wrapper");
        assert_eq!(
            coblet_edit_set_main_class(edit, main.as_ptr(), null),
            CobletStatus::Ok
        );
        let agent = text("-javaagent:/opt/fix.jar");
        assert_eq!(
            coblet_edit_add_arg(edit, CobletArgKind::Jvm, agent.as_ptr(), true, null),
            CobletStatus::Ok
        );
        let (name, path) = (text("acme:fix:1.0"), text("acme/fix/1.0/fix-1.0.jar"));
        assert_eq!(
            coblet_edit_add_library(
                edit,
                name.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                true,
                null
            ),
            CobletStatus::Ok
        );
        let escape = text("../escape.jar");
        assert_eq!(
            coblet_edit_add_library(
                edit,
                name.as_ptr(),
                escape.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                false,
                null
            ),
            CobletStatus::InvalidArgument
        );
        let asm = text("org.ow2.asm:asm-all:4.1");
        let mut removed = false;
        assert_eq!(
            coblet_edit_remove_library(edit, asm.as_ptr(), &raw mut removed, null),
            CobletStatus::Ok
        );
        assert!(removed);
        let (env, one) = (text("ACME"), text("1"));
        assert_eq!(
            coblet_edit_set_env(edit, env.as_ptr(), one.as_ptr(), null),
            CobletStatus::Ok
        );
        let (anchor, step_name) = (text("probe"), text("acme-probe"));
        let step = CobletStep {
            name: step_name.as_ptr(),
            user_data,
            run: Some(run_step),
            free: Some(free_step),
        };
        assert_eq!(
            coblet_edit_add_step(edit, anchor.as_ptr(), false, step, null),
            CobletStatus::Ok
        );
    }
    true
}

unsafe extern "C" fn free_customizer(user_data: *mut c_void) {
    state(user_data).frees.fetch_add(1, Ordering::SeqCst);
}

fn table(state: &State, id: &CStr) -> CobletCustomizer {
    CobletCustomizer {
        id: id.as_ptr(),
        display_name: std::ptr::null(),
        user_data: std::ptr::from_ref(state).cast_mut().cast(),
        applies_to: Some(applies_to),
        customize: Some(customize),
        free: Some(free_customizer),
    }
}

fn platform() -> Platform {
    Platform::new(OsFamily::Linux, Arch::X86_64)
}

fn profile() -> EffectiveLaunchProfile {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/mojang/c0.30_01c.json");
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
    ProfileEngine::builtin()
        .resolve(&resolved, &platform())
        .unwrap()
}

fn adapter(state: &State) -> ForeignCustomizer {
    let id = CString::new("acme:fix").unwrap();
    // SAFETY: the table is valid and `state` outlives the adapter.
    unsafe { ForeignCustomizer::new(&table(state, &id)) }.unwrap()
}

fn customize_with(
    customizer: &ForeignCustomizer,
    profile: &mut EffectiveLaunchProfile,
) -> Result<CustomizeCtx, LaunchError> {
    let ctx = CustomizeCtx::new(
        platform(),
        GameLayout::new("/data", "/game"),
        profile.traits.clone(),
    );
    coblet::launch::LaunchCustomizer::customize(customizer, profile, &ctx)?;
    Ok(ctx)
}

#[test]
fn edits_reach_the_profile_and_free_runs_once() {
    let state = State::default();
    let customizer = adapter(&state);
    let mut profile = profile();
    assert!(coblet::launch::LaunchCustomizer::applies_to(
        &customizer,
        &profile.traits
    ));
    let ctx = customize_with(&customizer, &mut profile).unwrap();
    assert_eq!(profile.main_class, "acme.Wrapper");
    assert_eq!(profile.jvm_args[0].value, "-javaagent:/opt/fix.jar");
    assert_eq!(profile.libraries[0].name.to_string(), "acme:fix:1.0");
    assert_eq!(ctx.extra_steps(), ["acme-probe"]);
    drop(ctx);
    assert_eq!(state.step_frees.load(Ordering::SeqCst), 1);
    assert_eq!(state.frees.load(Ordering::SeqCst), 0);
    drop(customizer);
    assert_eq!(state.frees.load(Ordering::SeqCst), 1);
}

#[test]
fn a_false_return_becomes_a_launch_error() {
    let state = State {
        fail: true,
        ..State::default()
    };
    let customizer = adapter(&state);
    let mut profile = profile();
    let before = profile.clone();
    let error = customize_with(&customizer, &mut profile).unwrap_err();
    assert_eq!(
        error.to_string(),
        "step `acme:fix` failed: agent jar is missing"
    );
    assert_eq!(profile, before);
}

#[test]
fn rejected_tables_are_still_freed() {
    let state = State::default();
    let good = CString::new("acme:fix").unwrap();
    let bad = CString::new("No Namespace").unwrap();
    let root = CString::new("/tmp/coblet-ffi-customizers").unwrap();
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
        let status = coblet_builder_add_customizer(builder, table(&state, &good), &raw mut err);
        assert_eq!(status, CobletStatus::Ok);
        let status = coblet_builder_add_customizer(builder, table(&state, &bad), &raw mut err);
        assert_eq!(status, CobletStatus::InvalidArgument);
        let message = CStr::from_ptr((*err).message.ptr).to_str().unwrap();
        assert!(message.starts_with("customizer id"), "{message}");
        crate::context::coblet_error_free(err);
        assert_eq!(state.frees.load(Ordering::SeqCst), 1);
        coblet_builder_free(builder);
    }
    assert_eq!(state.frees.load(Ordering::SeqCst), 2);
}

struct Offline;

impl Transport for Offline {
    fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(std::future::pending())
    }
}

struct Anchor;

impl coblet::launch::LaunchStep for Anchor {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn run<'a>(&'a self, _cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async { Ok(()) })
    }
}

fn launch(state: &State) -> Result<LaunchContext, LaunchError> {
    let mut launcher = Launcher::empty().with_step(Arc::new(Anchor));
    launcher
        .register_customizer(Arc::new(adapter(state)))
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = std::env::temp_dir().join(format!("coblet-ffi-{}", std::process::id()));
    let storage = Arc::new(TokioStorage::new());
    let transport: Arc<dyn Transport> = Arc::new(Offline);
    let meta = MetaCache::new(Arc::clone(&transport), storage.clone(), dir.join("meta"));
    let services = LaunchServices::new(
        storage.clone(),
        Arc::new(TokioSpawner::new()),
        Downloader::new(transport, storage, DownloaderConfig::default()),
        Arc::new(CatalogContext::new(meta, CachePolicy::PreferCache)),
        OsFamily::Linux,
    );
    let request = LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
        .with_customizer(ExtensionId::new("acme:fix").unwrap());
    let mut cx = LaunchContext::new(
        services,
        profile(),
        GameLayout::new("/data", "/game"),
        request,
    );
    runtime.block_on(launcher.run(&mut cx))?;
    Ok(cx)
}

#[test]
fn c_steps_run_in_the_launch() {
    let state = State::default();
    let cx = launch(&state).unwrap();
    assert_eq!(state.step_runs.load(Ordering::SeqCst), 1);
    let info = state.last_info.lock().unwrap().clone();
    assert!(info.contains("\"game_dir\":\"/game\""), "{info}");
    assert!(info.contains("\"java\":null"), "{info}");
    assert!(
        cx.profile
            .explain()
            .contains("[customize] acme:fix: env ACME set to \"1\"")
    );
    assert_eq!(state.step_frees.load(Ordering::SeqCst), 1);
}
