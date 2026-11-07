use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use coblet::catalog::CatalogContext;
use coblet::mojang::{
    BUILTIN_TRAITS_TOML, TraitContext, TraitError, TraitReport, TraitTable, TraitTarget,
    VersionTrait, VersionTraits,
};
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::{ExtensionId, VersionType};

use crate::builder::{CobletBuilder, builder_mut};
use crate::context::Coblet;
use crate::customize::Owned;
use crate::guard::{guard, put};
use crate::types::{CobletBytes, CobletError, CobletStatus, CobletString, Failure, text};

/// Flag of `coblet_builder_add_trait_source` and the table forms: put the
/// source in place of the one with the same ID, keeping its place, instead
/// of failing.
pub const COBLET_TRAITS_REPLACE: u32 = 1;

/// Callback table of a version trait source written in C.
///
/// `id` and `name` are copied when the table is added. `adjust` runs once
/// per batch on a library worker thread where blocking is fine: once with
/// every listed version when the version list is loaded, and once with the
/// version being launched. It gets a `CobletTraitCtx` handle, valid only
/// during the call, and the batch as JSON: `{"schema":1,"versions":[{"id",
/// "type","released","source","version","traits"}]}` where `version` is
/// the version JSON, present only for a launch, and `type`, `released` and
/// `source` may be null.
///
/// It fetches documents with `coblet_trait_ctx_fetch`, which goes through
/// the metadata cache and the mirror rules, and answers with
/// `coblet_trait_ctx_set_result`: a JSON array holding the new trait list
/// of every version, in the order given. Returning true without an answer
/// keeps the traits as they are. On failure it returns false, optionally
/// after `coblet_trait_ctx_fail`; the library then keeps the traits as they
/// were and reports a `version.trait-source-failed` warning. The library
/// owns `user_data` from the call that receives the table, even when it
/// fails, and calls `free` once when the `Coblet` is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletTraitSource {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:fixes`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// Changes the traits of the batch in `versions_json`. Returns false on failure.
    pub adjust: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletTraitCtx,
            versions_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// One running trait source callback: its requests and its answer.
pub struct CobletTraitCtx {
    catalog: CatalogContext,
    source: ExtensionId,
    runtime: tokio::runtime::Handle,
    result: Option<String>,
    failure: Option<String>,
}

impl std::fmt::Debug for CobletTraitCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletTraitCtx")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

type Callback = unsafe extern "C" fn(*mut c_void, *mut CobletTraitCtx, *const c_char) -> bool;

/// C trait source used as a coblet trait source.
pub(crate) struct ForeignTraitSource {
    id: ExtensionId,
    name: String,
    callback: Callback,
    owned: Arc<Owned>,
}

impl ForeignTraitSource {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    unsafe fn new(table: &CobletTraitSource) -> Result<Self, Failure> {
        let owned = Arc::new(Owned {
            user_data: table.user_data,
            free: table.free,
        });
        let callback = table
            .adjust
            .ok_or_else(|| Failure::argument("trait source", "has no callback"))?;
        // SAFETY: forwarded from the caller's guarantee.
        let (id, name) = unsafe { (text(table.id, "id")?, text(table.name, "name")?) };
        Ok(Self {
            id: extension_id(id)?,
            name: name.to_owned(),
            callback,
            owned,
        })
    }
}

impl Extension for ForeignTraitSource {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

#[allow(unsafe_code, reason = "calls a C trait source callback")]
fn invoke(owned: &Owned, callback: Callback, ctx: &mut CobletTraitCtx, argument: &CString) -> bool {
    // SAFETY: the table owner promised a callback that accepts its user
    // data, the context handle and a NUL-terminated string, all valid for
    // the duration of the call.
    unsafe { callback(owned.user_data, ctx, argument.as_ptr()) }
}

fn target_json(target: &TraitTarget) -> serde_json::Value {
    let input = target.input();
    let version = input
        .version
        .as_ref()
        .and_then(|document| serde_json::from_slice::<serde_json::Value>(&document.bytes).ok());
    serde_json::json!({
        "id": input.id.as_str(),
        "type": input.version_type.as_ref().map(VersionType::as_str),
        "released": input.released.map(|time| time.to_string()),
        "source": input.source.as_ref().map(ExtensionId::as_str),
        "version": version,
        "traits": trait_names(target.traits()),
    })
}

fn trait_names(traits: &VersionTraits) -> Vec<&str> {
    traits.iter().map(VersionTrait::as_str).collect()
}

/// Parses the answer of a C trait source: one trait list per version.
fn parse_answer(json: &str, count: usize) -> Result<Vec<VersionTraits>, TraitError> {
    let lists: Vec<Vec<String>> = serde_json::from_str(json)
        .map_err(|error| TraitError::failed(format!("answer: {error}")))?;
    if lists.len() != count {
        return Err(TraitError::failed(format!(
            "returned {} trait lists for {count} versions",
            lists.len()
        )));
    }
    lists
        .into_iter()
        .map(|names| {
            names
                .into_iter()
                .map(|name| {
                    if name.is_empty() || name.chars().any(char::is_whitespace) {
                        Err(TraitError::failed(format!("invalid trait name `{name}`")))
                    } else {
                        Ok(VersionTrait::from(name))
                    }
                })
                .collect()
        })
        .collect()
}

impl coblet::mojang::TraitSource for ForeignTraitSource {
    fn adjust<'a>(
        &'a self,
        cx: &'a TraitContext<'a>,
        versions: &'a mut [TraitTarget],
    ) -> BoxFuture<'a, Result<(), TraitError>> {
        Box::pin(async move {
            let batch: Vec<_> = versions.iter().map(target_json).collect();
            let argument = serde_json::json!({ "schema": 1, "versions": batch }).to_string();
            let argument =
                CString::new(argument).map_err(|_| TraitError::failed("batch contains NUL"))?;
            let catalog = cx.catalog();
            let mut ctx = CobletTraitCtx {
                catalog: CatalogContext::new(catalog.meta().clone(), catalog.policy()),
                source: self.id.clone(),
                runtime: tokio::runtime::Handle::current(),
                result: None,
                failure: None,
            };
            let (owned, callback) = (Arc::clone(&self.owned), self.callback);
            let (succeeded, ctx) = tokio::task::spawn_blocking(move || {
                let succeeded = invoke(&owned, callback, &mut ctx, &argument);
                (succeeded, ctx)
            })
            .await
            .map_err(|error| TraitError::failed(format!("the callback panicked: {error}")))?;
            for warning in ctx.catalog.take_warnings() {
                catalog.warn(warning);
            }
            if !succeeded {
                return Err(TraitError::failed(
                    ctx.failure
                        .unwrap_or_else(|| "the callback failed".to_owned()),
                ));
            }
            let Some(json) = ctx.result else {
                return Ok(());
            };
            let parsed = parse_answer(&json, versions.len())?;
            for (target, traits) in versions.iter_mut().zip(parsed) {
                *target.traits_mut() = traits;
            }
            Ok(())
        })
    }
}

fn extension_id(id: &str) -> Result<ExtensionId, Failure> {
    ExtensionId::new(id).map_err(|error| Failure::argument("trait source id", &error.to_string()))
}

/// Reads a context handle argument.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(ctx: *mut CobletTraitCtx) -> Result<&'a mut CobletTraitCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Fetches a document for the running trait source callback and caches it
/// under `cache_name`, a relative path such as `traits.json`; blocks until
/// it is there.
///
/// The request goes through the metadata cache, so mirror rules and
/// fallback apply. On success `*out_body` receives bytes to free with
/// `coblet_bytes_free`. An address that is not HTTP or HTTPS or an unsafe
/// cache name fails, a document that is unavailable and not cached with
/// `COBLET_ERROR_KIND_NETWORK`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; strings must be NULL or NUL-terminated; `out_body`
/// must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_trait_ctx_fetch(
    ctx: *mut CobletTraitCtx,
    url: *const c_char,
    cache_name: *const c_char,
    out_body: *mut CobletBytes,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let (url, name) = (text(url, "url")?, text(cache_name, "cache_name")?);
            if out_body.is_null() {
                return Err(Failure::argument("out_body", "is NULL"));
            }
            let cx = TraitContext::new(&ctx.catalog, &ctx.source);
            // The callback runs on a blocking thread, outside the runtime's
            // own tasks, so waiting here is allowed.
            let body = ctx
                .runtime
                .block_on(cx.fetch(url, name))
                .map_err(|error| Failure::from(coblet::Error::from(error)))?;
            put(out_body, CobletBytes::new(body.to_vec()), "out_body")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Hands the answer of the running trait source callback to the library,
/// which copies it: a JSON array with the new trait list of every version.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `json` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_trait_ctx_set_result(
    ctx: *mut CobletTraitCtx,
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

/// Gives the reason the running trait source callback is about to return false.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_trait_ctx_fail(
    ctx: *mut CobletTraitCtx,
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

/// One version trait source change, applied in order on top of the built-in list.
pub(crate) enum TraitChange {
    Add(Arc<dyn coblet::mojang::TraitSource>, bool),
    Remove(ExtensionId),
}

impl TraitChange {
    /// Applies the change to the engine builder.
    pub(crate) fn apply(
        self,
        builder: coblet::CobletBuilder<coblet::WithRoot>,
    ) -> coblet::CobletBuilder<coblet::WithRoot> {
        match self {
            Self::Add(source, false) => builder.trait_source(source),
            Self::Add(source, true) => builder.replace_trait_source(source),
            Self::Remove(id) => builder.without_trait_source(id),
        }
    }
}

/// Reads the flags of the trait source registration calls.
fn replace(flags: u32) -> Result<bool, Failure> {
    if flags & !COBLET_TRAITS_REPLACE != 0 {
        return Err(Failure::argument(
            "flags",
            &format!("unknown flags {flags:#x}"),
        ));
    }
    Ok(flags & COBLET_TRAITS_REPLACE != 0)
}

/// Adds a trait source change to a builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder handle; `out_err` must be NULL
/// or writable.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn push(
    builder: *mut CobletBuilder,
    out_err: *mut *mut CobletError,
    change: impl FnOnce() -> Result<TraitChange, Failure>,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            builder.traits.push(change()?);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a version trait source written in C, run after the built-in table
/// and the sources already added, so it can remove traits they gave.
///
/// `flags` is 0 or `COBLET_TRAITS_REPLACE`. The library owns the table's
/// `user_data` from this call on, even when it fails. A table without its
/// callback, a malformed ID or an unknown flag fails with
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
pub unsafe extern "C" fn coblet_builder_add_trait_source(
    builder: *mut CobletBuilder,
    source: CobletTraitSource,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignTraitSource::new(&source) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let replace = replace(flags)?;
            Ok(TraitChange::Add(Arc::new(adapter?), replace))
        })
    }
}

/// Adds a version trait table written in TOML, in the format of the
/// built-in table (`coblet_builtin_version_traits_toml`). `flags` is as
/// for `coblet_builder_add_trait_source`; `COBLET_TRAITS_REPLACE` with the
/// ID `coblet:version-traits` swaps the built-in table. A malformed table
/// fails with `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `table` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_trait_table_toml(
    builder: *mut CobletBuilder,
    table: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let replace = replace(flags)?;
            let table = TraitTable::from_toml(text(table, "table")?)
                .map_err(|error| Failure::argument("table", &error.to_string()))?;
            Ok(TraitChange::Add(Arc::new(table), replace))
        })
    }
}

/// Adds a version trait table written in JSON, with the keys of the TOML
/// form of `coblet_builder_add_trait_table_toml`.
///
/// # Safety
///
/// As `coblet_builder_add_trait_table_toml`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_trait_table_json(
    builder: *mut CobletBuilder,
    table: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let replace = replace(flags)?;
            let table = TraitTable::from_json(text(table, "table")?)
                .map_err(|error| Failure::argument("table", &error.to_string()))?;
            Ok(TraitChange::Add(Arc::new(table), replace))
        })
    }
}

/// Removes a version trait source, such as the built-in table
/// `coblet:version-traits`; an unknown ID is ignored.
///
/// # Safety
///
/// `builder` must be a live builder handle; `id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_trait_source(
    builder: *mut CobletBuilder,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            Ok(TraitChange::Remove(extension_id(text(id, "id")?)?))
        })
    }
}

/// Reads a `Coblet` handle argument.
///
/// # Safety
///
/// `coblet` must be NULL or a live handle.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn handle<'a>(coblet: *const Coblet) -> Result<&'a Coblet, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { coblet.as_ref() }.ok_or_else(|| Failure::argument("coblet", "is NULL"))
}

/// Hands a JSON object out with `"schema":1` added.
///
/// # Safety
///
/// `out_json` must be NULL or writable.
#[allow(unsafe_code, reason = "writes the out-parameter")]
unsafe fn put_json(
    out_json: *mut CobletString,
    mut json: serde_json::Value,
) -> Result<CobletStatus, Failure> {
    if let Some(map) = json.as_object_mut() {
        map.insert("schema".into(), 1.into());
    }
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { put(out_json, CobletString::new(&json.to_string()), "out_json") }?;
    Ok(CobletStatus::Ok)
}

fn report_json(report: &TraitReport) -> serde_json::Value {
    let changes: Vec<_> = report
        .changes
        .iter()
        .map(|change| {
            serde_json::json!({
                "source": change.source.as_str(),
                "action": change.action.as_str(),
                "value": change.value.as_str(),
            })
        })
        .collect();
    serde_json::json!({ "traits": trait_names(&report.traits), "changes": changes })
}

/// Lists the version trait sources, in the order they run, as JSON:
/// `{"schema":1,"sources":[{"id","name"}]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_trait_sources_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let sources: Vec<_> = handle
                .inner
                .trait_sources()
                .iter()
                .map(|source| {
                    serde_json::json!({
                        "id": source.id().as_str(),
                        "name": source.display_name(),
                    })
                })
                .collect();
            put_json(out_json, serde_json::json!({ "sources": sources }))
        })
    }
}

/// Returns the traits of a version with its version JSON, and which trait
/// source added or removed each, as JSON: `{"schema":1,"traits":[...],
/// "changes":[{"source","action","value"}]}` with `action` `added` or
/// `removed`. Sources that fail are skipped and reported as
/// `version.trait-source-failed` warnings. An unknown version fails with
/// `COBLET_ERROR_KIND_NOT_FOUND`. On success `*out_json` receives a string
/// to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `version` must be NULL or
/// NUL-terminated; `out_json` must be writable; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_version_traits_json(
    coblet: *const Coblet,
    version: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let version = text(version, "version")?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let report = handle.runtime.block_on(async {
                let version = handle.inner.version(version).await?;
                handle.inner.version_traits(&version).await
            })?;
            put_json(out_json, report_json(&report))
        })
    }
}

/// Hands out the text of the built-in version trait table, to copy into a
/// table that replaces it under the ID `coblet:version-traits`. On success
/// `*out_toml` receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `out_toml` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builtin_version_traits_toml(
    out_toml: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            put(out_toml, CobletString::new(BUILTIN_TRAITS_TOML), "out_toml")?;
            Ok(CobletStatus::Ok)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use coblet::runtime::{BoxFuture, MemoryBody, Request, Response, Transport, TransportError};
    use coblet::types::{Arch, OsFamily, Platform};

    use super::{
        COBLET_TRAITS_REPLACE, CobletTraitCtx, CobletTraitSource, ForeignTraitSource,
        coblet_builder_add_trait_source, coblet_builder_add_trait_table_json,
        coblet_builder_add_trait_table_toml, coblet_builder_remove_trait_source,
        coblet_builtin_version_traits_toml, coblet_trait_ctx_fail, coblet_trait_ctx_fetch,
        coblet_trait_ctx_set_result, coblet_trait_sources_json, coblet_version_traits_json,
    };
    use crate::accounts::coblet_bytes_free;
    use crate::builder::{coblet_builder_build, coblet_builder_free, coblet_builder_new};
    use crate::context::{Coblet, coblet_error_free, coblet_free, coblet_string_free};
    use crate::types::{CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString};

    /// Serves the Mojang fixtures by file name and a remote trait list.
    struct Fixtures;

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
                if url == "https://traits.fake.test/traits.json" {
                    let body = r#"{"b1.7.3": ["acme:remote"]}"#;
                    return Ok(Response::new(200, Vec::new(), MemoryBody::new(body)));
                }
                let name = url.rsplit('/').next().unwrap_or_default();
                let dir =
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/mojang");
                Ok(match std::fs::read(dir.join(name)) {
                    Ok(bytes) => Response::new(200, Vec::new(), MemoryBody::new(bytes)),
                    Err(_) => Response::new(404, Vec::new(), MemoryBody::new(Vec::new())),
                })
            })
        }
    }

    /// Counts frees in the `AtomicUsize` that `user_data` points to, if any.
    unsafe extern "C" fn free(user_data: *mut c_void) {
        // SAFETY: the tests pass NULL or a counter that outlives the handle.
        if let Some(count) = unsafe { user_data.cast::<AtomicUsize>().as_ref() } {
            count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Batches with a version JSON seen by `remote`.
    static LAUNCH_BATCHES: AtomicUsize = AtomicUsize::new(0);

    /// Adds the traits of a remote list and, for a launch, drops
    /// `legacy-macos-rendering`.
    unsafe extern "C" fn remote(
        _user_data: *mut c_void,
        ctx: *mut CobletTraitCtx,
        batch: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated JSON.
        let batch: serde_json::Value =
            serde_json::from_str(unsafe { CStr::from_ptr(batch) }.to_str().unwrap()).unwrap();
        let mut body = CobletBytes::empty();
        let mut err = std::ptr::null_mut();
        // SAFETY: the running callback's handle, valid strings and out-pointers.
        let status = unsafe {
            coblet_trait_ctx_fetch(
                ctx,
                c"https://traits.fake.test/traits.json".as_ptr(),
                c"traits.json".as_ptr(),
                &raw mut body,
                &raw mut err,
            )
        };
        assert_eq!(status, CobletStatus::Ok);
        // SAFETY: the bytes were just returned and are freed once.
        let listed: serde_json::Value = unsafe {
            let bytes = std::slice::from_raw_parts(body.ptr, body.len);
            let value = serde_json::from_slice(bytes).unwrap();
            coblet_bytes_free(body);
            value
        };
        let answer: Vec<Vec<String>> = batch["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|version| {
                let mut traits: Vec<String> = version["traits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| name.as_str().unwrap().to_owned())
                    .collect();
                if let Some(extra) = listed[version["id"].as_str().unwrap()].as_array() {
                    traits.extend(extra.iter().map(|name| name.as_str().unwrap().to_owned()));
                }
                if version["version"].is_object() {
                    assert_eq!(version["version"]["id"], "b1.7.3");
                    LAUNCH_BATCHES.fetch_add(1, Ordering::SeqCst);
                    traits.retain(|name| name != "legacy-macos-rendering");
                }
                traits
            })
            .collect();
        let json = CString::new(serde_json::to_string(&answer).unwrap()).unwrap();
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_trait_ctx_set_result(ctx, json.as_ptr()) == CobletStatus::Ok }
    }

    unsafe extern "C" fn refuse(
        _user_data: *mut c_void,
        ctx: *mut CobletTraitCtx,
        _batch: *const c_char,
    ) -> bool {
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_trait_ctx_fail(ctx, c"no data".as_ptr()) };
        false
    }

    /// Answers true without a result, which keeps the traits.
    unsafe extern "C" fn keep(
        _user_data: *mut c_void,
        _ctx: *mut CobletTraitCtx,
        _batch: *const c_char,
    ) -> bool {
        true
    }

    fn table(id: &'static CStr, user_data: *mut c_void) -> CobletTraitSource {
        CobletTraitSource {
            user_data,
            id: id.as_ptr(),
            name: c"Remote traits".as_ptr(),
            adjust: Some(remote),
            free: Some(free),
        }
    }

    /// A handle with the fixtures transport and the C sources.
    fn coblet(root: &std::path::Path) -> *mut Coblet {
        let broken = CobletTraitSource {
            adjust: Some(refuse),
            ..table(c"test:broken", std::ptr::null_mut())
        };
        let quiet = CobletTraitSource {
            adjust: Some(keep),
            ..table(c"test:quiet", std::ptr::null_mut())
        };
        // SAFETY: the tables hold static strings.
        let sources = unsafe {
            [
                ForeignTraitSource::new(&table(c"test:remote", std::ptr::null_mut())).unwrap(),
                ForeignTraitSource::new(&broken).unwrap(),
                ForeignTraitSource::new(&quiet).unwrap(),
            ]
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let inner = runtime.block_on(async {
            let mut builder = coblet::Coblet::builder()
                .data_root(root)
                .transport(Arc::new(Fixtures))
                .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64));
            for source in sources {
                builder = builder.trait_source(Arc::new(source));
            }
            builder.build().unwrap()
        });
        Box::into_raw(Box::new(Coblet { runtime, inner }))
    }

    fn take(out: &mut CobletString) -> serde_json::Value {
        let value = std::mem::replace(out, CobletString::empty());
        // SAFETY: the string was just returned and is freed once.
        unsafe {
            let text = CStr::from_ptr(value.ptr).to_str().unwrap().to_owned();
            coblet_string_free(value);
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        }
    }

    fn take_error(err: *mut CobletError) -> CobletErrorKind {
        assert!(!err.is_null());
        // SAFETY: the error was just returned and is freed once.
        unsafe {
            let kind = (*err).kind;
            coblet_error_free(err);
            kind
        }
    }

    #[test]
    fn c_sources_tag_listed_and_launched_versions() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(dir.path());
        let mut out = CobletString::empty();
        let mut err = std::ptr::null_mut();
        // SAFETY: a live handle and valid out-pointers.
        unsafe {
            let status = coblet_trait_sources_json(coblet, &raw mut out, &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let ids: Vec<String> = take(&mut out)["sources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|source| source["id"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                ids,
                [
                    "coblet:version-traits",
                    "test:remote",
                    "test:broken",
                    "test:quiet"
                ]
            );

            let status = crate::context::coblet_versions_json(coblet, &raw mut out, &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let versions = take(&mut out);
            let beta = versions["versions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|version| version["id"] == "b1.7.3")
                .unwrap()
                .clone();
            assert_eq!(
                beta["traits"],
                serde_json::json!(["legacy-online-services", "acme:remote"])
            );
            assert_eq!(beta["source"], "coblet:mojang");

            let status =
                coblet_version_traits_json(coblet, c"b1.7.3".as_ptr(), &raw mut out, &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let report = take(&mut out);
            assert_eq!(report["schema"], 1);
            assert_eq!(
                report["traits"],
                serde_json::json!(["jre-legacy", "legacy-online-services", "acme:remote"])
            );
            assert!(
                report["changes"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!({
                        "source": "test:remote",
                        "action": "removed",
                        "value": "legacy-macos-rendering"
                    }))
            );
            assert_eq!(LAUNCH_BATCHES.load(Ordering::SeqCst), 1);

            let status =
                coblet_version_traits_json(coblet, c"9.9".as_ptr(), &raw mut out, &raw mut err);
            assert_ne!(status, CobletStatus::Ok);
            assert_eq!(take_error(err), CobletErrorKind::NotFound);
            coblet_free(coblet);
        }
    }

    fn new_builder(root: &std::path::Path) -> *mut crate::builder::CobletBuilder {
        let root = CString::new(root.to_str().unwrap()).unwrap();
        let mut builder = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        // SAFETY: valid strings and out-pointers.
        let status = unsafe {
            coblet_builder_new(
                root.as_ptr(),
                c"coblet-ffi-test/1.0".as_ptr(),
                &raw mut builder,
                &raw mut err,
            )
        };
        assert_eq!(status, CobletStatus::Ok);
        builder
    }

    #[test]
    fn the_builder_checks_tables_flags_and_ids() {
        let dir = tempfile::tempdir().unwrap();
        let frees = AtomicUsize::new(0);
        let counter = std::ptr::from_ref(&frees).cast_mut().cast::<c_void>();
        let mut err = std::ptr::null_mut();
        let mut out = CobletString::empty();
        // SAFETY: live builders, tables with static strings, valid out-pointers.
        unsafe {
            let builder = new_builder(dir.path());
            let status = coblet_builder_add_trait_table_toml(
                builder,
                c"id = \"acme:bad\"\n[[rule]]\nids = [\"1.0\"]".as_ptr(),
                0,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::InvalidArgument);
            assert_eq!(take_error(err), CobletErrorKind::InvalidArgument);
            let status = coblet_builder_add_trait_source(
                builder,
                table(c"acme:flags", counter),
                4,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
            // The rejected table was freed at once.
            assert_eq!(frees.load(Ordering::SeqCst), 1);

            let status = coblet_builtin_version_traits_toml(&raw mut out, &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let builtin = CString::new(take(&mut out).as_str().unwrap()).unwrap();
            let status = coblet_builder_add_trait_table_toml(
                builder,
                builtin.as_ptr(),
                COBLET_TRAITS_REPLACE,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            let status =
                coblet_builder_remove_trait_source(builder, c"no-namespace".as_ptr(), &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
            for json in [
                cr#"{"id": "acme:fixes", "rule": [{"add": ["x"]}]}"#,
                cr#"{"id": "acme:fixes", "rule": [{"add": ["y"]}]}"#,
            ] {
                let status =
                    coblet_builder_add_trait_table_json(builder, json.as_ptr(), 0, &raw mut err);
                assert_eq!(status, CobletStatus::Ok);
            }
            let mut coblet = std::ptr::null_mut();
            let status = coblet_builder_build(builder, &raw mut coblet, &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            assert_eq!(take_error(err), CobletErrorKind::InvalidArgument);

            let builder = new_builder(dir.path());
            let status = coblet_builder_add_trait_source(
                builder,
                table(c"acme:kept", counter),
                0,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            coblet_builder_free(builder);
            assert_eq!(frees.load(Ordering::SeqCst), 2);
        }
    }
}
