use std::collections::BTreeMap;
use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use coblet::launch::{LaunchContext, LaunchError, PlaceholderOrigin, step_names};
use coblet::runtime::BoxFuture;

use crate::builder::{CobletBuilder, builder_mut};
use crate::context::Coblet;
use crate::customize::{Owned, c_text, trait_names};
use crate::guard::{guard, put};
use crate::placeholders::check_name;
use crate::types::{CobletError, CobletStatus, CobletString, Failure, text};

/// Callback table of a launch step written in C.
///
/// The library copies `name` and owns `user_data` from the call that
/// receives the table, even when that call fails, and calls `free` once
/// when it no longer needs it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletStep {
    /// Step name such as `acme-start-proxy`.
    pub name: *const c_char,
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Runs the step on a library worker thread, where blocking is fine.
    ///
    /// `info_json` is `{"schema":1,"game_version","traits":[],"data_root",
    /// "game_dir","java","java_major","natives_dir","client_jar",
    /// "game_assets","jvm_args":[],"placeholders":{}}`, with `null` for
    /// values not known yet. What the step sets with the
    /// `coblet_step_ctx_*` functions takes effect for the later steps when
    /// it returns true. Returning false stops the launch; call
    /// `coblet_step_ctx_fail` first to give the reason.
    pub run: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletStepCtx,
            info_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// What a running C step set, applied to the launch when it returns true;
/// valid only during its `run` callback.
#[derive(Debug, Default)]
pub struct CobletStepCtx {
    java: Option<PathBuf>,
    natives_dir: Option<PathBuf>,
    client_jar: Option<PathBuf>,
    game_assets: Option<PathBuf>,
    jvm_args: Vec<String>,
    placeholders: BTreeMap<String, String>,
    failure: Option<String>,
}

type Run = unsafe extern "C" fn(*mut c_void, *mut CobletStepCtx, *const c_char) -> bool;

/// C step run as a Rust launch step.
pub(crate) struct ForeignStep {
    name: String,
    run: Run,
    owned: Arc<Owned>,
}

impl ForeignStep {
    /// Takes ownership of the table's user data and checks the table.
    ///
    /// # Safety
    ///
    /// `name` must be NULL or NUL-terminated; the callbacks must stay valid
    /// until `free` runs.
    #[allow(unsafe_code, reason = "reads the callback table")]
    pub(crate) unsafe fn new(table: &CobletStep) -> Result<Self, Failure> {
        let owned = Arc::new(Owned {
            user_data: table.user_data,
            free: table.free,
        });
        // SAFETY: forwarded from the caller's guarantee.
        let name = unsafe { text(table.name, "step.name") }?.to_owned();
        let run = table
            .run
            .ok_or_else(|| Failure::argument("step.run", "is NULL"))?;
        Ok(Self { name, run, owned })
    }

    async fn apply(&self, ctx: CobletStepCtx, cx: &mut LaunchContext) -> Result<(), LaunchError> {
        if let Some(java) = ctx.java {
            cx.use_java(&java).await?;
        }
        if let Some(dir) = ctx.natives_dir {
            cx.natives_directory = Some(dir);
        }
        if let Some(jar) = ctx.client_jar {
            cx.client_jar = Some(jar);
        }
        if let Some(assets) = ctx.game_assets {
            cx.game_assets = Some(assets);
        }
        cx.jvm_args.extend(ctx.jvm_args);
        for (name, value) in ctx.placeholders {
            cx.placeholders
                .set(name, value, PlaceholderOrigin::Step(self.name.clone()))?;
        }
        Ok(())
    }
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn info(cx: &LaunchContext) -> serde_json::Value {
    let placeholders: serde_json::Map<String, serde_json::Value> = cx
        .placeholders
        .names()
        .filter_map(|name| Some((name.to_owned(), cx.placeholders.get(name)?.into())))
        .collect();
    serde_json::json!({
        "schema": 1,
        "game_version": cx.profile.game_version.as_str(),
        "traits": trait_names(&cx.profile.traits),
        "data_root": lossy(cx.layout.data_root()),
        "game_dir": lossy(cx.layout.game_directory()),
        "java": cx.java.as_ref().map(|java| lossy(&java.executable)),
        "java_major": cx.java.as_ref().map(|java| java.major),
        "natives_dir": cx.natives_directory.as_deref().map(lossy),
        "client_jar": cx.client_jar.as_deref().map(lossy),
        "game_assets": cx.game_assets.as_deref().map(lossy),
        "jvm_args": cx.jvm_args,
        "placeholders": placeholders,
    })
}

impl coblet::launch::LaunchStep for ForeignStep {
    fn name(&self) -> &str {
        &self.name
    }

    #[allow(unsafe_code, reason = "calls the C callback")]
    fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
        let json = c_text(&info(cx).to_string());
        let run = self.run;
        let owned = Arc::clone(&self.owned);
        Box::pin(async move {
            let failed = |message: String| LaunchError::Step {
                step: self.name.clone(),
                message,
            };
            let (ok, ctx) = tokio::task::spawn_blocking(move || {
                let mut ctx = CobletStepCtx::default();
                // SAFETY: the callback and user data are valid until `free`,
                // which `owned` delays; the string and context live for the call.
                let ok = unsafe { run(owned.user_data, &raw mut ctx, json.as_ptr()) };
                (ok, ctx)
            })
            .await
            .map_err(|error| failed(format!("the step stopped: {error}")))?;
            if !ok {
                return Err(failed(
                    ctx.failure.unwrap_or_else(|| "the step failed".to_owned()),
                ));
            }
            self.apply(ctx, cx).await
        })
    }
}

/// Reads the context handle of a running step.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(ctx: *mut CobletStepCtx) -> Result<&'a mut CobletStepCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Reads an absolute path argument.
///
/// # Safety
///
/// `path` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "reads a C string")]
unsafe fn absolute(path: *const c_char) -> Result<PathBuf, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    let path = PathBuf::from(unsafe { text(path, "path") }?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Failure::argument("path", "must be absolute"))
    }
}

/// Sets one path of the running step.
///
/// # Safety
///
/// As for the exported setters.
#[allow(unsafe_code, reason = "reads handle and string arguments")]
unsafe fn set_path(
    ctx: *mut CobletStepCtx,
    path: *const c_char,
    out_err: *mut *mut CobletError,
    slot: fn(&mut CobletStepCtx) -> &mut Option<PathBuf>,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            *slot(ctx) = Some(absolute(path)?);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Uses the `java` at the absolute `path` for this launch, as a
/// replacement of `select-java` does. After the step returns the library
/// runs it and checks it fits the version; the launch fails otherwise.
///
/// # Safety
///
/// `ctx` must be the handle of the running `run` callback; `path` must be
/// NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_ctx_set_java(
    ctx: *mut CobletStepCtx,
    path: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { set_path(ctx, path, out_err, |ctx| &mut ctx.java) }
}

/// Sets the natives directory, an absolute path, as a replacement of
/// `prepare-natives` does.
///
/// # Safety
///
/// As for `coblet_step_ctx_set_java`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_ctx_set_natives_dir(
    ctx: *mut CobletStepCtx,
    path: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { set_path(ctx, path, out_err, |ctx| &mut ctx.natives_dir) }
}

/// Starts the game from the client jar at the absolute `path`, as a
/// replacement of `client-jar` does.
///
/// # Safety
///
/// As for `coblet_step_ctx_set_java`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_ctx_set_client_jar(
    ctx: *mut CobletStepCtx,
    path: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { set_path(ctx, path, out_err, |ctx| &mut ctx.client_jar) }
}

/// Sets `${game_assets}` to the absolute `path`, as a replacement of
/// `verify-files` does for old asset indexes.
///
/// # Safety
///
/// As for `coblet_step_ctx_set_java`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_ctx_set_game_assets(
    ctx: *mut CobletStepCtx,
    path: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { set_path(ctx, path, out_err, |ctx| &mut ctx.game_assets) }
}

/// Adds a JVM argument after those of the version; it may use `${name}`
/// placeholders.
///
/// # Safety
///
/// `ctx` must be the handle of the running `run` callback; `value` must
/// be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_ctx_add_jvm_arg(
    ctx: *mut CobletStepCtx,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            ctx.jvm_args.push(text(value, "value")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// Sets `${name}` to `value` for the rest of the launch. A name coblet
/// fills itself, such as `auth_access_token`, an empty name or one with
/// `$`, `{` or `}` fails with `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `ctx` must be the handle of the running `run` callback; strings must be
/// NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_ctx_set_placeholder(
    ctx: *mut CobletStepCtx,
    name: *const c_char,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let name = check_name(text(name, "name")?)?;
            ctx.placeholders
                .insert(name, text(value, "value")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// Gives the reason the running `run` callback is about to return false.
///
/// # Safety
///
/// `ctx` must be the handle of the running `run` callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_ctx_fail(
    ctx: *mut CobletStepCtx,
    message: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            ctx_mut(ctx)?.failure = Some(text(message, "message")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// One launch step or placeholder change, applied in order.
pub(crate) enum LaunchChange {
    Placeholder(String, String),
    Source(Arc<dyn coblet::launch::PlaceholderSource>, bool),
    RemoveSource(coblet::types::ExtensionId),
    Unknown(coblet::launch_command::UnknownPlaceholder),
    Step(StepPlace, String, Arc<ForeignStep>),
    RemoveStep(String),
}

/// Where a step added with the builder goes.
pub(crate) enum StepPlace {
    Before,
    After,
    Replace,
}

impl LaunchChange {
    /// Applies the change to the engine builder.
    pub(crate) fn apply(
        self,
        builder: coblet::CobletBuilder<coblet::WithRoot>,
    ) -> coblet::CobletBuilder<coblet::WithRoot> {
        match self {
            Self::Placeholder(name, value) => builder.placeholder(name, value),
            Self::Source(source, false) => builder.placeholder_source(source),
            Self::Source(source, true) => builder.replace_placeholder_source(source),
            Self::RemoveSource(id) => builder.without_placeholder_source(id),
            Self::Unknown(policy) => builder.unknown_placeholders(policy),
            Self::Step(StepPlace::Before, anchor, step) => builder.launch_step_before(anchor, step),
            Self::Step(StepPlace::After, anchor, step) => builder.launch_step_after(anchor, step),
            Self::Step(StepPlace::Replace, name, step) => builder.replace_launch_step(name, step),
            Self::RemoveStep(name) => builder.without_launch_step(name),
        }
    }
}

/// Adds a launch change to a builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder handle; `out_err` must be NULL
/// or writable.
#[allow(unsafe_code, reason = "reads a handle argument")]
pub(crate) unsafe fn push(
    builder: *mut CobletBuilder,
    out_err: *mut *mut CobletError,
    change: impl FnOnce() -> Result<LaunchChange, Failure>,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            builder.launch.push(change()?);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Refuses to replace or remove the steps only Rust code can stand in for.
fn replaceable(name: &str) -> Result<(), Failure> {
    if name == step_names::BUILD_COMMAND || name == step_names::SPAWN {
        return Err(Failure::argument(
            "name",
            &format!(
                "step `{name}` makes the command or the game process, which only Rust code can, so it cannot be replaced or removed here"
            ),
        ));
    }
    Ok(())
}

/// Runs `step` in every launch right before, or with `after` right after,
/// the step named `anchor`, such as `build-command`. Changes apply in the
/// order they are made; `coblet_builder_build` fails when no step has that
/// name.
///
/// The library owns the table's `user_data` from this call on, even when
/// it fails.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated; the step's callbacks must stay valid until its `free`
/// runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_step(
    builder: *mut CobletBuilder,
    anchor: *const c_char,
    after: bool,
    step: CobletStep,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table; the adapter owns the user
    // data from here on.
    let adapter = unsafe { ForeignStep::new(&step) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let anchor = text(anchor, "anchor")?.to_owned();
            let place = if after {
                StepPlace::After
            } else {
                StepPlace::Before
            };
            Ok(LaunchChange::Step(place, anchor, Arc::new(adapter?)))
        })
    }
}

/// Runs `step` instead of the step named `name`, such as `client-jar`;
/// the new step must leave what later steps read, through its
/// `CobletStepCtx`. `build-command` and `spawn` make the command and the
/// game process, which only Rust code can, so replacing them fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`; `coblet_builder_build` fails when no
/// step has that name.
///
/// The library owns the table's `user_data` from this call on, even when
/// it fails.
///
/// # Safety
///
/// As for `coblet_builder_add_step`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_replace_step(
    builder: *mut CobletBuilder,
    name: *const c_char,
    step: CobletStep,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table; the adapter owns the user
    // data from here on.
    let adapter = unsafe { ForeignStep::new(&step) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let name = text(name, "name")?.to_owned();
            replaceable(&name)?;
            Ok(LaunchChange::Step(
                StepPlace::Replace,
                name,
                Arc::new(adapter?),
            ))
        })
    }
}

/// Leaves out the step named `name`, such as `logging-config`. Removing
/// `build-command` or `spawn`, without which no game starts, fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`; `coblet_builder_build` fails when no
/// step has that name.
///
/// # Safety
///
/// `builder` must be a live builder handle; `name` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_step(
    builder: *mut CobletBuilder,
    name: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let name = text(name, "name")?.to_owned();
            replaceable(&name)?;
            Ok(LaunchChange::RemoveStep(name))
        })
    }
}

/// Lists the launch steps in the order they run as JSON:
/// `{"schema":1,"steps":["select-java",...]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_launch_steps_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            let json = serde_json::json!({
                "schema": 1,
                "steps": handle.inner.launcher().names(),
            });
            put(out_json, CobletString::new(&json.to_string()), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use coblet::launch::{LaunchContext, LaunchError, LaunchRequest, Launcher, PlaceholderOrigin};
    use coblet::launch_command::LaunchAuth;
    use coblet::types::PlayerName;

    use super::{
        CobletStep, CobletStepCtx, ForeignStep, coblet_builder_add_step,
        coblet_builder_remove_step, coblet_builder_replace_step, coblet_launch_steps_json,
        coblet_step_ctx_add_jvm_arg, coblet_step_ctx_fail, coblet_step_ctx_set_client_jar,
        coblet_step_ctx_set_natives_dir, coblet_step_ctx_set_placeholder,
    };
    use crate::builder::{coblet_builder_build, coblet_builder_free, coblet_builder_new};
    use crate::context::{coblet_error_free, coblet_free, coblet_string_free};
    use crate::testing::launch_context;
    use crate::types::{CobletErrorKind, CobletStatus, CobletString};

    /// State behind `user_data`, as a C program would keep it.
    #[derive(Default)]
    struct State {
        fail: bool,
        runs: AtomicUsize,
        frees: AtomicUsize,
        info: std::sync::Mutex<String>,
    }

    fn state(user_data: *mut c_void) -> &'static State {
        // SAFETY: every test passes a pointer to a `State` that outlives its use.
        unsafe { &*user_data.cast::<State>() }
    }

    unsafe extern "C" fn run(
        user_data: *mut c_void,
        ctx: *mut CobletStepCtx,
        info_json: *const c_char,
    ) -> bool {
        let state = state(user_data);
        state.runs.fetch_add(1, Ordering::SeqCst);
        let null = std::ptr::null_mut();
        // SAFETY: the library passes a NUL-terminated string and the context
        // handle of this call; the strings live for each call.
        unsafe {
            let info = CStr::from_ptr(info_json).to_str().unwrap();
            info.clone_into(&mut state.info.lock().unwrap());
            assert_eq!(
                coblet_step_ctx_set_client_jar(ctx, c"/opt/client.jar".as_ptr(), null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_step_ctx_set_natives_dir(ctx, c"natives".as_ptr(), null),
                CobletStatus::InvalidArgument
            );
            assert_eq!(
                coblet_step_ctx_set_natives_dir(ctx, c"/opt/natives".as_ptr(), null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_step_ctx_add_jvm_arg(ctx, c"-Dacme=${acme_port}".as_ptr(), null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_step_ctx_set_placeholder(
                    ctx,
                    c"acme_port".as_ptr(),
                    c"25565".as_ptr(),
                    null
                ),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_step_ctx_set_placeholder(ctx, c"auth_uuid".as_ptr(), c"x".as_ptr(), null),
                CobletStatus::InvalidArgument
            );
            if state.fail {
                coblet_step_ctx_fail(ctx, c"mirror is down".as_ptr());
                return false;
            }
        }
        true
    }

    unsafe extern "C" fn free(user_data: *mut c_void) {
        state(user_data).frees.fetch_add(1, Ordering::SeqCst);
    }

    fn table(state: &State, name: &'static CStr) -> CobletStep {
        CobletStep {
            name: name.as_ptr(),
            user_data: std::ptr::from_ref(state).cast_mut().cast(),
            run: Some(run),
            free: Some(free),
        }
    }

    async fn launch(dir: &Path, state: &State) -> (Result<(), LaunchError>, LaunchContext) {
        // SAFETY: the table has a static name and a valid callback.
        let step = unsafe { ForeignStep::new(&table(state, c"acme-prepare")) }.unwrap();
        let launcher = Launcher::empty().with_step(Arc::new(step));
        let request = LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
            .with_placeholder("acme_asked", "yes");
        let mut cx = launch_context(dir, "1.20.1", request);
        let result = launcher.run(&mut cx).await;
        (result, cx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn what_a_c_step_sets_reaches_the_launch_when_it_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::default();
        let (result, cx) = launch(dir.path(), &state).await;
        result.unwrap();
        assert_eq!(cx.client_jar.as_deref(), Some(Path::new("/opt/client.jar")));
        assert_eq!(
            cx.natives_directory.as_deref(),
            Some(Path::new("/opt/natives"))
        );
        assert_eq!(cx.jvm_args, ["-Dacme=${acme_port}"]);
        assert_eq!(
            cx.placeholders.origin("acme_port"),
            Some(&PlaceholderOrigin::Step("acme-prepare".to_owned()))
        );
        let info: serde_json::Value = serde_json::from_str(&state.info.lock().unwrap()).unwrap();
        assert_eq!(info["game_version"], "1.20.1");
        assert_eq!(info["placeholders"]["acme_asked"], "yes");
        assert!(info["java"].is_null());
        drop(cx);
        assert_eq!(state.frees.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_c_step_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = State {
            fail: true,
            ..State::default()
        };
        let (result, cx) = launch(dir.path(), &state).await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "step `acme-prepare` failed: mirror is down"
        );
        assert!(cx.client_jar.is_none() && cx.jvm_args.is_empty());
        assert_eq!(cx.placeholders.get("acme_port"), None);
    }

    #[test]
    fn the_builder_adds_replaces_and_removes_steps() {
        let dir = tempfile::tempdir().unwrap();
        let root = CString::new(dir.path().to_str().unwrap()).unwrap();
        let state = State::default();
        let mut err = std::ptr::null_mut();
        let null = std::ptr::null_mut();
        // SAFETY: live builders, tables with static strings, valid out-pointers.
        unsafe {
            let new_builder = || {
                let mut builder = std::ptr::null_mut();
                let status = coblet_builder_new(
                    root.as_ptr(),
                    c"coblet-ffi-test/1.0".as_ptr(),
                    &raw mut builder,
                    std::ptr::null_mut(),
                );
                assert_eq!(status, CobletStatus::Ok);
                builder
            };
            let builder = new_builder();
            let step = table(&state, c"acme-proxy");
            assert_eq!(
                coblet_builder_add_step(builder, c"build-command".as_ptr(), false, step, null),
                CobletStatus::Ok
            );
            let step = table(&state, c"acme-jar");
            assert_eq!(
                coblet_builder_replace_step(builder, c"client-jar".as_ptr(), step, null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_remove_step(builder, c"logging-config".as_ptr(), null),
                CobletStatus::Ok
            );
            for name in [c"build-command", c"spawn"] {
                assert_eq!(
                    coblet_builder_remove_step(builder, name.as_ptr(), null),
                    CobletStatus::InvalidArgument
                );
                let step = table(&state, c"acme-x");
                assert_eq!(
                    coblet_builder_replace_step(builder, name.as_ptr(), step, null),
                    CobletStatus::InvalidArgument
                );
            }
            assert_eq!(state.frees.load(Ordering::SeqCst), 2);
            let mut coblet = std::ptr::null_mut();
            assert_eq!(
                coblet_builder_build(builder, &raw mut coblet, null),
                CobletStatus::Ok
            );
            let mut json = CobletString::empty();
            assert_eq!(
                coblet_launch_steps_json(coblet, &raw mut json, null),
                CobletStatus::Ok
            );
            let text = CStr::from_ptr(json.ptr).to_str().unwrap().to_owned();
            let steps: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                steps["steps"],
                serde_json::json!([
                    "select-java",
                    "verify-files",
                    "prepare-natives",
                    "acme-jar",
                    "legacy-fml-libs",
                    "pre-launch",
                    "resolve-placeholders",
                    "acme-proxy",
                    "build-command",
                    "spawn"
                ])
            );
            coblet_string_free(json);
            coblet_free(coblet);
            assert_eq!(state.frees.load(Ordering::SeqCst), 4);

            let builder = new_builder();
            assert_eq!(
                coblet_builder_remove_step(builder, c"acme-missing".as_ptr(), null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_build(builder, &raw mut coblet, &raw mut err),
                CobletStatus::Error
            );
            assert_eq!((*err).kind, CobletErrorKind::Launch);
            coblet_error_free(err);
            let builder = new_builder();
            coblet_builder_free(builder);
        }
    }
}
