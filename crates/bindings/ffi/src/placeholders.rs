use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use coblet::launch::{LaunchError, PlaceholderCtx};
use coblet::launch_command::UnknownPlaceholder;
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::ExtensionId;

use crate::builder::CobletBuilder;
use crate::context::Coblet;
use crate::customize::{Owned, trait_names};
use crate::guard::{guard, put};
use crate::steps::{LaunchChange, push};
use crate::types::{CobletError, CobletStatus, CobletString, Failure, text};

/// Flag of `coblet_builder_add_placeholder_source`: put the source in
/// place of the one with the same ID, keeping its place, instead of failing.
pub const COBLET_PLACEHOLDER_REPLACE: u32 = 1;

/// What happens to a `${name}` placeholder nothing fills.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletUnknownPlaceholders {
    /// The launch fails.
    Error = 0,
    /// `${name}` stays as written.
    Keep = 1,
    /// It is replaced with nothing.
    Empty = 2,
}

/// Callback table of a placeholder source written in C, asked for
/// `${name}` placeholders nothing else in a launch set.
///
/// `id` and `name` are copied when the table is added. Sources are asked
/// in order, each only for the names still missing. `resolve` runs on a
/// library worker thread where blocking is fine, with a `CobletPlaceholderCtx`
/// handle valid only during the call and the request as JSON:
/// `{"schema":1,"names":[],"game_version","traits":[],"os","arch",
/// "data_root","game_dir","java_major"}` where `java_major` may be null.
/// It answers with `coblet_placeholder_ctx_set_result`, a JSON object of
/// the names it knows and their values; returning true without an answer
/// knows none. Returning false stops the launch; call
/// `coblet_placeholder_ctx_fail` first to give the reason. The library
/// owns `user_data` from the call that receives the table, even when it
/// fails, and calls `free` once when the `Coblet` is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletPlaceholderSource {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:server`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// Answers the request in `request_json`. Returns false on failure.
    pub resolve: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletPlaceholderCtx,
            request_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// One running placeholder source callback and its answer.
#[derive(Debug, Default)]
pub struct CobletPlaceholderCtx {
    result: Option<String>,
    failure: Option<String>,
}

type Resolve = unsafe extern "C" fn(*mut c_void, *mut CobletPlaceholderCtx, *const c_char) -> bool;

/// C placeholder source used as a coblet placeholder source.
pub(crate) struct ForeignPlaceholderSource {
    id: ExtensionId,
    name: String,
    resolve: Resolve,
    owned: Arc<Owned>,
}

impl ForeignPlaceholderSource {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    unsafe fn new(table: &CobletPlaceholderSource) -> Result<Self, Failure> {
        let owned = Arc::new(Owned {
            user_data: table.user_data,
            free: table.free,
        });
        let resolve = table
            .resolve
            .ok_or_else(|| Failure::argument("placeholder source", "has no callback"))?;
        // SAFETY: forwarded from the caller's guarantee.
        let (id, name) = unsafe { (text(table.id, "id")?, text(table.name, "name")?) };
        let id = ExtensionId::new(id)
            .map_err(|error| Failure::argument("placeholder source id", &error.to_string()))?;
        Ok(Self {
            id,
            name: name.to_owned(),
            resolve,
            owned,
        })
    }
}

impl Extension for ForeignPlaceholderSource {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

#[allow(unsafe_code, reason = "calls a C placeholder source callback")]
fn invoke(
    owned: &Owned,
    resolve: Resolve,
    ctx: &mut CobletPlaceholderCtx,
    request: &CString,
) -> bool {
    // SAFETY: the table owner promised a callback that accepts its user
    // data, the context handle and a NUL-terminated string, all valid for
    // the duration of the call.
    unsafe { resolve(owned.user_data, ctx, request.as_ptr()) }
}

fn request_json(names: &[String], cx: &PlaceholderCtx) -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "names": names,
        "game_version": cx.game_version.as_str(),
        "traits": trait_names(&cx.traits),
        "os": cx.platform.os().as_str(),
        "arch": cx.platform.arch().as_str(),
        "data_root": cx.layout.data_root().to_string_lossy(),
        "game_dir": cx.layout.game_directory().to_string_lossy(),
        "java_major": cx.java_major,
    })
}

impl coblet::launch::PlaceholderSource for ForeignPlaceholderSource {
    fn resolve<'a>(
        &'a self,
        names: &'a [String],
        cx: &'a PlaceholderCtx,
    ) -> BoxFuture<'a, Result<BTreeMap<String, String>, LaunchError>> {
        Box::pin(async move {
            let failed = |message: String| LaunchError::PlaceholderSource {
                source_id: self.id.clone(),
                message,
            };
            let request = CString::new(request_json(names, cx).to_string())
                .map_err(|_| failed("the request contains NUL".to_owned()))?;
            let (owned, resolve) = (Arc::clone(&self.owned), self.resolve);
            let (succeeded, ctx) = tokio::task::spawn_blocking(move || {
                let mut ctx = CobletPlaceholderCtx::default();
                let succeeded = invoke(&owned, resolve, &mut ctx, &request);
                (succeeded, ctx)
            })
            .await
            .map_err(|error| failed(format!("the callback stopped: {error}")))?;
            if !succeeded {
                return Err(failed(
                    ctx.failure
                        .unwrap_or_else(|| "the callback failed".to_owned()),
                ));
            }
            let Some(json) = ctx.result else {
                return Ok(BTreeMap::new());
            };
            serde_json::from_str(&json).map_err(|error| failed(format!("answer: {error}")))
        })
    }
}

/// Checks a placeholder name the way the launcher will.
pub(crate) fn check_name(name: &str) -> Result<String, Failure> {
    coblet::launch::check_placeholder_name(name)
        .map_err(|error| Failure::argument("name", &error.to_string()))?;
    Ok(name.to_owned())
}

/// Reads a context handle argument.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(
    ctx: *mut CobletPlaceholderCtx,
) -> Result<&'a mut CobletPlaceholderCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Hands the answer of the running placeholder source callback to the
/// library, which copies it: a JSON object of names and values, such as
/// `{"acme_server":"play.acme.net"}`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `json` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_placeholder_ctx_set_result(
    ctx: *mut CobletPlaceholderCtx,
    json: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            ctx_mut(ctx)?.result = Some(text(json, "json")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// Gives the reason the running placeholder source callback is about to
/// return false.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_placeholder_ctx_fail(
    ctx: *mut CobletPlaceholderCtx,
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

/// Sets `${name}` to `value` in every launch; a customizer or step can
/// replace it. A name coblet fills itself, such as `auth_access_token`,
/// an empty name or one with `$`, `{` or `}` fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_placeholder(
    builder: *mut CobletBuilder,
    name: *const c_char,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let name = check_name(text(name, "name")?)?;
            Ok(LaunchChange::Placeholder(
                name,
                text(value, "value")?.to_owned(),
            ))
        })
    }
}

/// Adds a placeholder source written in C, asked after those already added.
///
/// `flags` is 0 or `COBLET_PLACEHOLDER_REPLACE`. The library owns the
/// table's `user_data` from this call on, even when it fails. A table
/// without its callback, a malformed ID or an unknown flag fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`; an ID already registered without the
/// flag fails `coblet_builder_build`.
///
/// # Safety
///
/// `builder` must be a live builder handle; the table's strings must be
/// NULL or NUL-terminated and its callbacks must stay valid until its
/// `free` runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_placeholder_source(
    builder: *mut CobletBuilder,
    source: CobletPlaceholderSource,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignPlaceholderSource::new(&source) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            if flags & !COBLET_PLACEHOLDER_REPLACE != 0 {
                return Err(Failure::argument(
                    "flags",
                    &format!("unknown flags {flags:#x}"),
                ));
            }
            let replace = flags & COBLET_PLACEHOLDER_REPLACE != 0;
            Ok(LaunchChange::Source(Arc::new(adapter?), replace))
        })
    }
}

/// Removes a placeholder source; an unknown ID is ignored.
///
/// # Safety
///
/// `builder` must be a live builder handle; `id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_placeholder_source(
    builder: *mut CobletBuilder,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let id = ExtensionId::new(text(id, "id")?)
                .map_err(|error| Failure::argument("id", &error.to_string()))?;
            Ok(LaunchChange::RemoveSource(id))
        })
    }
}

/// Sets what happens to placeholders nothing fills; the launch fails by default.
///
/// # Safety
///
/// `builder` must be a live builder handle; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_set_unknown_placeholders(
    builder: *mut CobletBuilder,
    policy: CobletUnknownPlaceholders,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    let policy = match policy {
        CobletUnknownPlaceholders::Error => UnknownPlaceholder::Error,
        CobletUnknownPlaceholders::Keep => UnknownPlaceholder::Keep,
        CobletUnknownPlaceholders::Empty => UnknownPlaceholder::Empty,
    };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { push(builder, out_err, || Ok(LaunchChange::Unknown(policy))) }
}

/// Lists the placeholder sources, in the order they are asked, as JSON:
/// `{"schema":1,"sources":[{"id","name"}]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_placeholder_sources_json(
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
            let sources: Vec<_> = handle
                .inner
                .placeholder_sources()
                .iter()
                .map(|source| {
                    serde_json::json!({
                        "id": source.id().as_str(),
                        "name": source.display_name(),
                    })
                })
                .collect();
            let json = serde_json::json!({ "schema": 1, "sources": sources });
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use coblet::launch::{
        LaunchError, LaunchRequest, Launcher, PlaceholderOrigin, PlaceholderSources,
        ResolvePlaceholders,
    };
    use coblet::launch_command::LaunchAuth;
    use coblet::types::{ExtensionId, PlayerName};

    use super::{
        COBLET_PLACEHOLDER_REPLACE, CobletPlaceholderCtx, CobletPlaceholderSource,
        CobletUnknownPlaceholders, ForeignPlaceholderSource, coblet_builder_add_placeholder,
        coblet_builder_add_placeholder_source, coblet_builder_remove_placeholder_source,
        coblet_builder_set_unknown_placeholders, coblet_placeholder_ctx_fail,
        coblet_placeholder_ctx_set_result, coblet_placeholder_sources_json,
    };
    use crate::builder::{coblet_builder_build, coblet_builder_new};
    use crate::context::{coblet_error_free, coblet_free, coblet_string_free};
    use crate::testing::launch_context;
    use crate::types::{CobletStatus, CobletString};

    /// State behind `user_data`, as a C program would keep it.
    struct State {
        answer: Option<&'static CStr>,
        asked: Mutex<Vec<String>>,
        frees: AtomicUsize,
    }

    impl State {
        fn new(answer: Option<&'static CStr>) -> Self {
            Self {
                answer,
                asked: Mutex::new(Vec::new()),
                frees: AtomicUsize::new(0),
            }
        }
    }

    fn state(user_data: *mut c_void) -> &'static State {
        // SAFETY: every test passes a pointer to a `State` that outlives its use.
        unsafe { &*user_data.cast::<State>() }
    }

    unsafe extern "C" fn resolve(
        user_data: *mut c_void,
        ctx: *mut CobletPlaceholderCtx,
        request_json: *const c_char,
    ) -> bool {
        let state = state(user_data);
        // SAFETY: the library passes a NUL-terminated string and the context
        // handle of this call.
        unsafe {
            let request = CStr::from_ptr(request_json).to_str().unwrap();
            state.asked.lock().unwrap().push(request.to_owned());
            let Some(answer) = state.answer else {
                coblet_placeholder_ctx_fail(ctx, c"server is down".as_ptr());
                return false;
            };
            assert_eq!(
                coblet_placeholder_ctx_set_result(ctx, answer.as_ptr()),
                CobletStatus::Ok
            );
        }
        true
    }

    unsafe extern "C" fn free(user_data: *mut c_void) {
        state(user_data).frees.fetch_add(1, Ordering::SeqCst);
    }

    fn table(state: &State, id: &'static CStr) -> CobletPlaceholderSource {
        CobletPlaceholderSource {
            user_data: std::ptr::from_ref(state).cast_mut().cast(),
            id: id.as_ptr(),
            name: c"Server".as_ptr(),
            resolve: Some(resolve),
            free: Some(free),
        }
    }

    async fn launch(
        dir: &Path,
        state: &State,
    ) -> Result<coblet::launch::LaunchContext, LaunchError> {
        // SAFETY: the table has static strings and a valid callback.
        let source = unsafe { ForeignPlaceholderSource::new(&table(state, c"acme:server")) };
        let mut sources = PlaceholderSources::new();
        sources.register(Arc::new(source.unwrap())).unwrap();
        let launcher = Launcher::empty().with_step(Arc::new(ResolvePlaceholders));
        let request = LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
            .with_placeholder("acme_user", "Steve");
        let mut cx = launch_context(dir, "1.20.1", request);
        cx.services.placeholder_sources = sources;
        cx.jvm_args
            .push("-Dacme=${acme_server}/${acme_user}".to_owned());
        launcher.run(&mut cx).await?;
        Ok(cx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_c_source_answers_the_names_still_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(Some(
            cr#"{"acme_server":"play.acme.net","acme_unasked":"x"}"#,
        ));
        let cx = launch(dir.path(), &state).await.unwrap();
        assert_eq!(cx.placeholders.get("acme_server"), Some("play.acme.net"));
        assert_eq!(cx.placeholders.get("acme_unasked"), None);
        assert_eq!(
            cx.placeholders.origin("acme_server"),
            Some(&PlaceholderOrigin::Source(
                ExtensionId::new("acme:server").unwrap()
            ))
        );
        let asked = state.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        let request: serde_json::Value = serde_json::from_str(&asked[0]).unwrap();
        assert_eq!(request["schema"], 1);
        assert_eq!(request["names"], serde_json::json!(["acme_server"]));
        assert_eq!(request["game_version"], "1.20.1");
        assert_eq!(request["os"], "linux");
        assert!(request["java_major"].is_null());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_or_garbled_c_source_stops_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let error = launch(dir.path(), &State::new(None)).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "placeholder source `acme:server` failed: server is down"
        );
        let error = launch(dir.path(), &State::new(Some(c"[1]")))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("placeholder source `acme:server` failed: answer:"),
            "{error}"
        );
    }

    #[test]
    fn the_builder_checks_names_flags_and_owns_the_tables() {
        let dir = tempfile::tempdir().unwrap();
        let root = CString::new(dir.path().to_str().unwrap()).unwrap();
        let state = State::new(Some(c"{}"));
        let mut builder = std::ptr::null_mut();
        let mut coblet = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        let null = std::ptr::null_mut();
        // SAFETY: a live builder, tables with static strings, valid out-pointers.
        unsafe {
            assert_eq!(
                coblet_builder_new(
                    root.as_ptr(),
                    c"coblet-ffi-test/1.0".as_ptr(),
                    &raw mut builder,
                    null,
                ),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_add_placeholder(
                    builder,
                    c"acme_brand".as_ptr(),
                    c"Acme".as_ptr(),
                    null
                ),
                CobletStatus::Ok
            );
            for name in [c"classpath", c"", c"a}b"] {
                assert_eq!(
                    coblet_builder_add_placeholder(builder, name.as_ptr(), c"x".as_ptr(), null),
                    CobletStatus::InvalidArgument
                );
            }
            let source = table(&state, c"acme:server");
            assert_eq!(
                coblet_builder_add_placeholder_source(builder, source, 2, null),
                CobletStatus::InvalidArgument
            );
            assert_eq!(state.frees.load(Ordering::SeqCst), 1);
            for flags in [0, COBLET_PLACEHOLDER_REPLACE] {
                let source = table(&state, c"acme:server");
                assert_eq!(
                    coblet_builder_add_placeholder_source(builder, source, flags, null),
                    CobletStatus::Ok
                );
            }
            let source = table(&state, c"acme:other");
            assert_eq!(
                coblet_builder_add_placeholder_source(builder, source, 0, null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_remove_placeholder_source(builder, c"acme:other".as_ptr(), null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_set_unknown_placeholders(
                    builder,
                    CobletUnknownPlaceholders::Keep,
                    null
                ),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_build(builder, &raw mut coblet, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(state.frees.load(Ordering::SeqCst), 3);
            let mut json = CobletString::empty();
            assert_eq!(
                coblet_placeholder_sources_json(coblet, &raw mut json, &raw mut err),
                CobletStatus::Ok
            );
            let text = CStr::from_ptr(json.ptr).to_str().unwrap().to_owned();
            coblet_string_free(json);
            let sources: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                sources,
                serde_json::json!({
                    "schema": 1,
                    "sources": [{"id": "acme:server", "name": "Server"}],
                })
            );
            coblet_free(coblet);
            assert_eq!(state.frees.load(Ordering::SeqCst), 4);
            coblet_error_free(err);
        }
    }
}
