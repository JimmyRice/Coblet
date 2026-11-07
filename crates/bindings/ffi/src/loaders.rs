use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use coblet::catalog::CatalogContext;
use coblet::loader::{DataLoader, InstallRecipe, LoaderError, LoaderVersion};
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::{ExtensionId, GameVersionId};

use crate::builder::{CobletBuilder, builder_mut};
use crate::context::Coblet;
use crate::customize::Owned;
use crate::guard::{guard, put};
use crate::types::{
    CobletBytes, CobletError, CobletStatus, CobletString, Failure, optional_text, text,
};

/// Flag of `coblet_builder_add_loader` and its data forms: put the loader in
/// place of the one registered under the same ID instead of failing.
pub const COBLET_LOADER_REPLACE: u32 = 1;

/// Callback table of a Mod Loader written in C.
///
/// `id`, `name` and `component_uid` are copied when the table is added.
/// Both callbacks run on a library worker thread where blocking is fine,
/// and get a `CobletLoaderCtx` handle, valid only during the call: they
/// fetch documents with `coblet_loader_ctx_fetch`, which goes through the
/// metadata cache and the mirror rules, hand their answer over with
/// `coblet_loader_ctx_set_result` and return true, or give a reason with
/// `coblet_loader_ctx_fail` and return false.
///
/// `versions` answers with a JSON array of versions, each
/// `{"id","game","type","released","recommended","data"}` where only `id`
/// is required: `type` defaults to `release`, `released` is RFC 3339 and
/// `data` is an object kept for `install`. `install` gets one such version
/// as JSON and answers with the install recipe, `{"components":[...]}` in
/// the Prism / MultiMC component format, the loader's own component among
/// them. The library owns `user_data` from the call that receives the
/// table, even when it fails, and calls `free` once when the `Coblet` is
/// freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletModLoader {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:loader`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// UID of the component the loader installs, such as `net.acme.loader`.
    pub component_uid: *const c_char,
    /// Lists versions for the game version `game`. Returns false on failure.
    pub versions: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletLoaderCtx,
            game: *const c_char,
        ) -> bool,
    >,
    /// Gives the install recipe of `version_json` for `game`. Returns false on failure.
    pub install: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletLoaderCtx,
            game: *const c_char,
            version_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// One running Mod Loader callback: its requests and its answer.
pub struct CobletLoaderCtx {
    catalog: CatalogContext,
    loader: ExtensionId,
    runtime: tokio::runtime::Handle,
    result: Option<String>,
    failure: Option<String>,
}

impl std::fmt::Debug for CobletLoaderCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletLoaderCtx")
            .field("loader", &self.loader)
            .finish_non_exhaustive()
    }
}

type Versions = unsafe extern "C" fn(*mut c_void, *mut CobletLoaderCtx, *const c_char) -> bool;
type Install =
    unsafe extern "C" fn(*mut c_void, *mut CobletLoaderCtx, *const c_char, *const c_char) -> bool;

/// C Mod Loader used as a coblet Mod Loader.
pub(crate) struct ForeignLoader {
    id: ExtensionId,
    name: String,
    uid: String,
    versions: Versions,
    install: Install,
    owned: Arc<Owned>,
}

impl ForeignLoader {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    pub(crate) unsafe fn new(table: &CobletModLoader) -> Result<Self, Failure> {
        let owned = Arc::new(Owned {
            user_data: table.user_data,
            free: table.free,
        });
        let (Some(versions), Some(install)) = (table.versions, table.install) else {
            return Err(Failure::argument(
                "mod loader",
                "needs versions and install callbacks",
            ));
        };
        // SAFETY: forwarded from the caller's guarantee.
        let (id, name, uid) = unsafe {
            (
                text(table.id, "loader id")?,
                text(table.name, "loader name")?,
                text(table.component_uid, "component uid")?,
            )
        };
        Ok(Self {
            id: loader_id(id)?,
            name: name.to_owned(),
            uid: uid.to_owned(),
            versions,
            install,
            owned,
        })
    }

    /// Runs a callback on a blocking thread with its own context, hands
    /// the context's warnings to `cx` and returns the callback's answer.
    async fn call(
        &self,
        cx: &coblet::loader::LoaderContext<'_>,
        game: &GameVersionId,
        version: Option<String>,
    ) -> Result<String, LoaderError> {
        let mut ctx = CobletLoaderCtx {
            catalog: CatalogContext::new(cx.catalog().meta().clone(), cx.catalog().policy()),
            loader: self.id.clone(),
            runtime: tokio::runtime::Handle::current(),
            result: None,
            failure: None,
        };
        let owned = Arc::clone(&self.owned);
        let (versions, install) = (self.versions, self.install);
        let game = CString::new(game.as_str())
            .map_err(|_| LoaderError::failed(&self.id, "game version contains NUL"))?;
        let version = version
            .map(CString::new)
            .transpose()
            .map_err(|_| LoaderError::failed(&self.id, "version contains NUL"))?;
        let joined = tokio::task::spawn_blocking(move || {
            let succeeded = invoke(&owned, &mut ctx, versions, install, &game, version.as_ref());
            (succeeded, ctx)
        })
        .await;
        let (succeeded, ctx) = joined.map_err(|error| {
            LoaderError::failed(&self.id, format!("the loader callback panicked: {error}"))
        })?;
        for warning in ctx.catalog.take_warnings() {
            cx.catalog().warn(warning);
        }
        match (succeeded, ctx.result) {
            (true, Some(result)) => Ok(result),
            (true, None) => Err(LoaderError::failed(
                &self.id,
                "the loader callback set no result",
            )),
            (false, _) => Err(LoaderError::failed(
                &self.id,
                ctx.failure
                    .unwrap_or_else(|| "the loader callback failed".to_owned()),
            )),
        }
    }
}

/// Calls `install` when there is a version, `versions` otherwise.
#[allow(unsafe_code, reason = "calls a C Mod Loader callback")]
fn invoke(
    owned: &Owned,
    ctx: &mut CobletLoaderCtx,
    versions: Versions,
    install: Install,
    game: &CString,
    version: Option<&CString>,
) -> bool {
    // SAFETY: the table owner promised callbacks that accept its user data,
    // the context handle and NUL-terminated strings, all valid for the
    // duration of the call.
    unsafe {
        match version {
            Some(version) => install(owned.user_data, ctx, game.as_ptr(), version.as_ptr()),
            None => versions(owned.user_data, ctx, game.as_ptr()),
        }
    }
}

impl Extension for ForeignLoader {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl coblet::loader::ModLoader for ForeignLoader {
    fn component_uid(&self) -> &str {
        &self.uid
    }

    fn versions<'a>(
        &'a self,
        cx: &'a coblet::loader::LoaderContext<'a>,
        game: &'a GameVersionId,
    ) -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>> {
        Box::pin(async move {
            let json = self.call(cx, game, None).await?;
            serde_json::from_str(&json)
                .map_err(|error| LoaderError::invalid(&self.id, format!("versions: {error}")))
        })
    }

    fn install<'a>(
        &'a self,
        cx: &'a coblet::loader::LoaderContext<'a>,
        game: &'a GameVersionId,
        version: &'a LoaderVersion,
    ) -> BoxFuture<'a, Result<InstallRecipe, LoaderError>> {
        Box::pin(async move {
            let version = serde_json::to_string(version)
                .map_err(|error| LoaderError::failed(&self.id, error.to_string()))?;
            let json = self.call(cx, game, Some(version)).await?;
            InstallRecipe::from_json(json.as_bytes())
                .map_err(|error| LoaderError::invalid(&self.id, format!("install recipe: {error}")))
        })
    }
}

fn loader_id(id: &str) -> Result<ExtensionId, Failure> {
    ExtensionId::new(id).map_err(|error| Failure::argument("loader id", &error.to_string()))
}

fn game_id(game: &str) -> Result<GameVersionId, Failure> {
    game.parse()
        .map_err(|error: coblet::types::GameVersionIdError| {
            Failure::argument("game", &error.to_string())
        })
}

/// Reads a context handle argument.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(ctx: *mut CobletLoaderCtx) -> Result<&'a mut CobletLoaderCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Fetches a document for the running Mod Loader callback and caches it
/// under `cache_name`, a relative path such as `versions/1.20.1.json`;
/// blocks until it is there.
///
/// The request goes through the metadata cache as Mod Loader metadata, so
/// mirror rules and fallback apply. On success `*out_body` receives bytes
/// to free with `coblet_bytes_free`. An address that is not HTTP or HTTPS
/// or an unsafe cache name fails with `COBLET_ERROR_KIND_OTHER`, a document
/// that is unavailable and not cached with `COBLET_ERROR_KIND_NETWORK`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; strings must be NULL or NUL-terminated; `out_body`
/// must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_loader_ctx_fetch(
    ctx: *mut CobletLoaderCtx,
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
            let cx = coblet::loader::LoaderContext::new(&ctx.catalog, &ctx.loader);
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

/// Hands the answer of the running Mod Loader callback to the library,
/// which copies it: the JSON array of versions or the recipe.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `json` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_loader_ctx_set_result(
    ctx: *mut CobletLoaderCtx,
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

/// Gives the reason the running Mod Loader callback is about to return false.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_loader_ctx_fail(
    ctx: *mut CobletLoaderCtx,
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

impl CobletBuilder {
    /// Adds a Mod Loader; `flags` may ask to replace one with the same ID.
    fn add_mod_loader(
        &mut self,
        loader: Arc<dyn coblet::loader::ModLoader>,
        flags: u32,
    ) -> Result<CobletStatus, Failure> {
        let replace = flags & COBLET_LOADER_REPLACE != 0;
        let taken = self
            .loaders
            .iter()
            .any(|(known, replaces)| !*replaces && known.id() == loader.id());
        if taken && !replace {
            return Err(Failure::argument(
                "mod loader",
                &format!("`{}` is already registered", loader.id()),
            ));
        }
        self.loaders.push((loader, replace));
        Ok(CobletStatus::Ok)
    }
}

fn check_flags(flags: u32) -> Result<(), Failure> {
    if flags & !COBLET_LOADER_REPLACE != 0 {
        return Err(Failure::argument(
            "flags",
            &format!("unknown flags {flags:#x}"),
        ));
    }
    Ok(())
}

/// Registers a Mod Loader written in C.
///
/// `flags` is 0 or `COBLET_LOADER_REPLACE`, which puts the loader in place
/// of the one registered under the same ID, such as a patched copy of a
/// built-in loader. The library owns the table's `user_data` from this call
/// on, even when it fails. A table without both callbacks, a malformed ID,
/// an ID already registered without the flag or an unknown flag fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; the table's strings must be
/// NULL or NUL-terminated and its callbacks must stay valid until its
/// `free` runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_loader(
    builder: *mut CobletBuilder,
    loader: CobletModLoader,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignLoader::new(&loader) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            check_flags(flags)?;
            builder.add_mod_loader(Arc::new(adapter?), flags)
        })
    }
}

/// Registers a Mod Loader described in TOML: where its versions are listed
/// and how a version becomes components, in the format of coblet's data
/// loaders. `flags` is as for `coblet_builder_add_loader`; a malformed
/// definition fails with `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `definition` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_loader_toml(
    builder: *mut CobletBuilder,
    definition: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            check_flags(flags)?;
            let loader = DataLoader::from_toml(text(definition, "definition")?)
                .map_err(|error| Failure::argument("definition", &error.to_string()))?;
            builder.add_mod_loader(Arc::new(loader), flags)
        })
    }
}

/// Registers a Mod Loader described in JSON, with the keys of the TOML form
/// of `coblet_builder_add_loader_toml`.
///
/// # Safety
///
/// As `coblet_builder_add_loader_toml`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_loader_json(
    builder: *mut CobletBuilder,
    definition: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            check_flags(flags)?;
            let loader = DataLoader::from_json(text(definition, "definition")?)
                .map_err(|error| Failure::argument("definition", &error.to_string()))?;
            builder.add_mod_loader(Arc::new(loader), flags)
        })
    }
}

/// Reads a `Coblet` argument.
///
/// # Safety
///
/// `coblet` must be NULL or a live handle.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn handle<'a>(coblet: *const Coblet) -> Result<&'a Coblet, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { coblet.as_ref() }.ok_or_else(|| Failure::argument("coblet", "is NULL"))
}

/// Hands a JSON document with `"schema":1` out.
///
/// # Safety
///
/// `out_json` must be writable.
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

/// Lists the Mod Loaders as JSON: `{"schema":1,"loaders":[{"id","name",
/// "component_uid"}]}`. On success `*out_json` receives a string to free
/// with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_loaders_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let loaders: Vec<serde_json::Value> = handle
                .inner
                .loaders()
                .iter()
                .map(|loader| {
                    serde_json::json!({
                        "id": loader.id().as_str(),
                        "name": loader.display_name(),
                        "component_uid": loader.component_uid(),
                    })
                })
                .collect();
            put_json(out_json, serde_json::json!({ "loaders": loaders }))
        })
    }
}

/// Lists the versions of a Mod Loader for a game version as JSON:
/// `{"schema":1,"versions":[{"id","game","type","released","recommended",
/// "data"}],"preselect"}`, where `preselect` is the ID to preselect (the
/// recommended version, otherwise the newest release) or null. On success
/// `*out_json` receives a string to free with `coblet_string_free`. An
/// unknown loader fails with `COBLET_ERROR_KIND_NOT_FOUND`.
///
/// # Safety
///
/// `coblet` must be a live handle; strings must be NULL or NUL-terminated;
/// `out_json` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_loader_versions_json(
    coblet: *const Coblet,
    loader: *const c_char,
    game: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let id = loader_id(text(loader, "loader")?)?;
            let game = game_id(text(game, "game")?)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let (versions, preselect) = handle.runtime.block_on(async {
                let versions = handle.inner.loader_versions(&id, &game).await?;
                let preselect = handle.inner.recommended_loader_version(&id, &game).await?;
                Ok::<_, coblet::Error>((versions, preselect))
            })?;
            put_json(
                out_json,
                serde_json::json!({
                    "versions": versions,
                    "preselect": preselect.map(|version| version.id),
                }),
            )
        })
    }
}

/// Returns the install recipe of a Mod Loader version as JSON:
/// `{"schema":1,"components":[...]}` in the Prism / MultiMC component
/// format. On success `*out_json` receives a string to free with
/// `coblet_string_free`. An unknown loader or version fails with
/// `COBLET_ERROR_KIND_NOT_FOUND`.
///
/// # Safety
///
/// `coblet` must be a live handle; strings must be NULL or NUL-terminated;
/// `out_json` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_loader_recipe_json(
    coblet: *const Coblet,
    loader: *const c_char,
    game: *const c_char,
    version: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let id = loader_id(text(loader, "loader")?)?;
            let game = game_id(text(game, "game")?)?;
            let version = text(version, "version")?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let recipe = handle.runtime.block_on(async {
                let chosen = handle.inner.loader_version(&id, &game, version).await?;
                handle.inner.loader_recipe(&id, &game, &chosen).await
            })?;
            let json = serde_json::to_value(&recipe).map_err(|error| {
                Failure::new(crate::types::CobletErrorKind::Other, error.to_string())
            })?;
            put_json(out_json, json)
        })
    }
}

/// Reads an optional loader choice: both NULL, or both set.
///
/// # Safety
///
/// Strings must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "reads string arguments")]
pub(crate) unsafe fn loader_choice(
    loader: *const c_char,
    version: *const c_char,
) -> Result<Option<(ExtensionId, String)>, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    let (loader, version) = unsafe {
        (
            optional_text(loader, "loader")?,
            optional_text(version, "loader_version")?,
        )
    };
    match (loader, version) {
        (None, None) => Ok(None),
        (Some(loader), Some(version)) => Ok(Some((loader_id(loader)?, version.to_owned()))),
        _ => Err(Failure::argument(
            "loader_version",
            "is needed exactly when a loader is given",
        )),
    }
}

/// Resolves a version for this platform, with a Mod Loader version on top
/// when `loader` is given, without downloading or starting anything, and
/// returns what the launch would run as JSON: `{"schema":1,"main_class",
/// "classpath":[maven coordinates],"jvm_args":[],"game_args":[],
/// "java_major","report"}`, arguments with their `${...}` placeholders.
///
/// `loader` and `loader_version` are both NULL or both set. On success
/// `*out_json` receives a string to free with `coblet_string_free`.
/// Unmet component requirements fail with `COBLET_ERROR_KIND_UNSUPPORTED`.
///
/// # Safety
///
/// `coblet` must be a live handle; strings must be NULL or NUL-terminated
/// (`version_id` not NULL); `out_json` must be writable; `out_err` must be
/// NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_plan_launch_json(
    coblet: *const Coblet,
    version_id: *const c_char,
    loader: *const c_char,
    loader_version: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let version_id = text(version_id, "version_id")?;
            let choice = loader_choice(loader, loader_version)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let profile = handle.runtime.block_on(async {
                let version = handle.inner.version(version_id).await?;
                let Some((id, chosen)) = choice else {
                    return handle.inner.resolve(&version).await;
                };
                let chosen = handle
                    .inner
                    .loader_version(&id, &version.id, &chosen)
                    .await?;
                let recipe = handle
                    .inner
                    .loader_recipe(&id, &version.id, &chosen)
                    .await?;
                handle
                    .inner
                    .resolve_with(&version, &recipe.components)
                    .await
            })?;
            let classpath: Vec<String> = profile
                .libraries
                .iter()
                .filter(|library| library.classpath.is_some())
                .map(|library| library.name.to_string())
                .collect();
            let args = |args: &[coblet::platform::ProfileArg]| -> Vec<String> {
                args.iter().map(|arg| arg.value.clone()).collect()
            };
            put_json(
                out_json,
                serde_json::json!({
                    "main_class": profile.main_class,
                    "classpath": classpath,
                    "jvm_args": args(&profile.jvm_args),
                    "game_args": args(&profile.game_args),
                    "java_major": profile.java.major,
                    "report": profile.explain(),
                }),
            )
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

    use coblet::runtime::{
        ArtifactKind, BoxFuture, MemoryBody, PrefixRewriter, Request, Response, Transport,
        TransportError,
    };
    use coblet::types::{Arch, OsFamily, Platform};

    use super::{
        COBLET_LOADER_REPLACE, CobletLoaderCtx, CobletModLoader, ForeignLoader,
        coblet_builder_add_loader, coblet_builder_add_loader_json, coblet_builder_add_loader_toml,
        coblet_loader_ctx_fail, coblet_loader_ctx_fetch, coblet_loader_ctx_set_result,
        coblet_loader_recipe_json, coblet_loader_versions_json, coblet_loaders_json,
        coblet_plan_launch_json,
    };
    use crate::accounts::coblet_bytes_free;
    use crate::builder::{coblet_builder_build, coblet_builder_new};
    use crate::context::{Coblet, coblet_error_free, coblet_free, coblet_string_free};
    use crate::types::{CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString};

    /// Serves the Mojang fixtures by file name and the fake listing from
    /// the mirror only.
    struct Fixtures;

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
                if url == "https://mirror.fake.test/meta/versions/1.20.1" {
                    let body = r#"[{ "id": "2.0", "type": "beta" }, { "id": "1.4.2", "recommended": true, "data": { "main": "test.fake.Knot" } }]"#;
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

    fn set(ctx: *mut CobletLoaderCtx, json: &str) -> bool {
        let json = CString::new(json).unwrap();
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_loader_ctx_set_result(ctx, json.as_ptr()) == CobletStatus::Ok }
    }

    /// Lists the versions served at the mirror of the fake metadata server.
    unsafe extern "C" fn versions(
        _user_data: *mut c_void,
        ctx: *mut CobletLoaderCtx,
        game: *const c_char,
    ) -> bool {
        // SAFETY: the library passes a NUL-terminated game version.
        let game = unsafe { CStr::from_ptr(game) }.to_str().unwrap();
        let url = CString::new(format!("https://meta.fake.test/versions/{game}")).unwrap();
        let name = CString::new(format!("versions/{game}.json")).unwrap();
        let mut body = CobletBytes::empty();
        let mut err = std::ptr::null_mut();
        // SAFETY: the running callback's handle, valid strings and out-pointers.
        let status = unsafe {
            coblet_loader_ctx_fetch(
                ctx,
                url.as_ptr(),
                name.as_ptr(),
                &raw mut body,
                &raw mut err,
            )
        };
        if status != CobletStatus::Ok {
            // SAFETY: the error was just returned.
            unsafe { coblet_error_free(err) };
            return false;
        }
        // SAFETY: the bytes were just returned and are freed once.
        let text = unsafe {
            let text = std::str::from_utf8(std::slice::from_raw_parts(body.ptr, body.len))
                .unwrap()
                .to_owned();
            coblet_bytes_free(body);
            text
        };
        set(ctx, &text)
    }

    /// Answers with a recipe built from the version JSON.
    unsafe extern "C" fn install(
        _user_data: *mut c_void,
        ctx: *mut CobletLoaderCtx,
        game: *const c_char,
        version: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated strings.
        let (game, version) = unsafe {
            (
                CStr::from_ptr(game).to_str().unwrap(),
                CStr::from_ptr(version).to_str().unwrap(),
            )
        };
        let version: serde_json::Value = serde_json::from_str(version).unwrap();
        let recipe = serde_json::json!({ "components": [{
            "uid": "test.fake.loader",
            "version": version["id"],
            "requires": [{ "uid": "net.minecraft", "equals": game }],
            "mainClass": version["data"]["main"],
            "+tweakers": ["test.fake.Tweaker"],
            "libraries": [{
                "name": format!("test.fake:fake-loader:{}", version["id"].as_str().unwrap()),
                "url": "https://maven.fake.test/"
            }]
        }]});
        set(ctx, &recipe.to_string())
    }

    unsafe extern "C" fn refuse(
        _user_data: *mut c_void,
        ctx: *mut CobletLoaderCtx,
        _game: *const c_char,
        _version: *const c_char,
    ) -> bool {
        let message = CString::new("installer offline").unwrap();
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_loader_ctx_fail(ctx, message.as_ptr()) };
        false
    }

    const ID: &CStr = c"test:fake-c";
    const NAME: &CStr = c"Fake C Loader";
    const UID: &CStr = c"test.fake.loader";

    fn table() -> CobletModLoader {
        CobletModLoader {
            user_data: std::ptr::null_mut(),
            id: ID.as_ptr(),
            name: NAME.as_ptr(),
            component_uid: UID.as_ptr(),
            versions: Some(versions),
            install: Some(install),
            free: Some(free),
        }
    }

    /// A handle with the fixtures transport, a mirror for Mod Loader
    /// metadata only, the fake C loader and one that refuses to install.
    fn coblet(root: &std::path::Path) -> *mut Coblet {
        let mirror =
            PrefixRewriter::new("https://meta.fake.test/", "https://mirror.fake.test/meta/")
                .unwrap()
                .for_kinds([ArtifactKind::Loader]);
        let broken = CobletModLoader {
            id: c"test:broken".as_ptr(),
            install: Some(refuse),
            ..table()
        };
        // SAFETY: the tables hold static strings.
        let (fake, broken) = unsafe {
            (
                ForeignLoader::new(&table()).unwrap(),
                ForeignLoader::new(&broken).unwrap(),
            )
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let inner = runtime.block_on(async {
            coblet::Coblet::builder()
                .data_root(root)
                .transport(Arc::new(Fixtures))
                .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
                .url_rewriter(Arc::new(mirror))
                .loader(Arc::new(fake))
                .loader(Arc::new(broken))
                .build()
                .unwrap()
        });
        Box::into_raw(Box::new(Coblet { runtime, inner }))
    }

    fn take(out: &mut CobletString) -> serde_json::Value {
        let value = std::mem::replace(out, CobletString::empty());
        // SAFETY: the string was just returned and is freed once.
        unsafe {
            let text = CStr::from_ptr(value.ptr).to_str().unwrap().to_owned();
            coblet_string_free(value);
            serde_json::from_str(&text).unwrap()
        }
    }

    fn take_error(err: *mut CobletError) -> (CobletErrorKind, String) {
        assert!(!err.is_null());
        // SAFETY: the error was just returned and is freed once.
        unsafe {
            let kind = (*err).kind;
            let message = CStr::from_ptr((*err).message.ptr)
                .to_str()
                .unwrap()
                .to_owned();
            coblet_error_free(err);
            (kind, message)
        }
    }

    #[test]
    fn c_loaders_list_install_and_plan_through_mirrors() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(dir.path());
        let game = c"1.20.1";
        let mut out = CobletString::empty();
        let mut err = std::ptr::null_mut();

        // SAFETY: a live handle, valid strings and out-pointers.
        unsafe {
            assert_eq!(
                coblet_loaders_json(coblet, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            let loaders = take(&mut out);
            assert_eq!(loaders["schema"], 1);
            assert!(
                loaders["loaders"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|loader| loader["id"] == "test:fake-c"
                        && loader["component_uid"] == "test.fake.loader")
            );

            let status = coblet_loader_versions_json(
                coblet,
                ID.as_ptr(),
                game.as_ptr(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            let versions = take(&mut out);
            assert_eq!(versions["versions"][0]["type"], "beta");
            assert_eq!(versions["versions"][1]["game"], "1.20.1");
            assert_eq!(versions["preselect"], "1.4.2");

            let status = coblet_loader_recipe_json(
                coblet,
                ID.as_ptr(),
                game.as_ptr(),
                c"1.4.2".as_ptr(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            let recipe = take(&mut out);
            assert_eq!(recipe["components"][0]["mainClass"], "test.fake.Knot");

            let status = coblet_plan_launch_json(
                coblet,
                game.as_ptr(),
                ID.as_ptr(),
                c"1.4.2".as_ptr(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            let plan = take(&mut out);
            assert_eq!(plan["main_class"], "test.fake.Knot");
            assert!(
                plan["classpath"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|name| name == "test.fake:fake-loader:1.4.2")
            );
            let args: Vec<&str> = plan["game_args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap())
                .collect();
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--tweakClass", "test.fake.Tweaker"])
            );

            let status = coblet_plan_launch_json(
                coblet,
                game.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            assert_eq!(take(&mut out)["java_major"], 17);
            coblet_free(coblet);
        }
    }

    #[test]
    fn c_loader_failures_have_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(dir.path());
        let game = c"1.20.1";
        let mut out = CobletString::empty();
        let mut err = std::ptr::null_mut();

        // SAFETY: a live handle, valid strings and out-pointers.
        unsafe {
            let status = coblet_loader_versions_json(
                coblet,
                c"test:missing".as_ptr(),
                game.as_ptr(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Error);
            assert_eq!(take_error(err).0, CobletErrorKind::NotFound);

            let status = coblet_loader_recipe_json(
                coblet,
                c"test:broken".as_ptr(),
                game.as_ptr(),
                c"1.4.2".as_ptr(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Error);
            let (kind, message) = take_error(err);
            assert_eq!(kind, CobletErrorKind::Other);
            assert!(message.contains("installer offline"), "{message}");

            let status = coblet_plan_launch_json(
                coblet,
                game.as_ptr(),
                ID.as_ptr(),
                std::ptr::null(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::InvalidArgument);
            assert_eq!(take_error(err).0, CobletErrorKind::InvalidArgument);
            coblet_free(coblet);
        }
    }

    const FAKE_DATA: &CStr = cr#"
        id = "test:fake-data"
        name = "Fake Loader"
        uid = "test.fake.loader"
        [versions]
        format = "static"
        list = [{ id = "1.0" }]
        [profile]
        format = "static"
        [[profile.components]]
        uid = "test.fake.loader"
        version = "{version}"
    "#;

    #[test]
    fn the_builder_keeps_the_registry_rules_and_frees_tables() {
        let dir = tempfile::tempdir().unwrap();
        let root = CString::new(dir.path().to_str().unwrap()).unwrap();
        let mut builder = std::ptr::null_mut();
        let mut coblet = std::ptr::null_mut();
        let mut out = CobletString::empty();
        let mut err = std::ptr::null_mut();
        let freed = AtomicUsize::new(0);
        let counted = || CobletModLoader {
            user_data: (&raw const freed).cast_mut().cast(),
            ..table()
        };

        // SAFETY: valid strings and out-pointers; the builder is consumed.
        unsafe {
            let status = coblet_builder_new(
                root.as_ptr(),
                c"coblet-ffi-test/1.0".as_ptr(),
                &raw mut builder,
                std::ptr::null_mut(),
            );
            assert_eq!(status, CobletStatus::Ok);
            let add = |definition: &CStr, flags: u32, err: *mut *mut CobletError| {
                coblet_builder_add_loader_toml(builder, definition.as_ptr(), flags, err)
            };
            assert_eq!(add(FAKE_DATA, 0, &raw mut err), CobletStatus::Ok);
            assert_eq!(
                add(FAKE_DATA, 0, &raw mut err),
                CobletStatus::InvalidArgument
            );
            assert!(take_error(err).1.contains("already registered"));
            assert_eq!(
                add(FAKE_DATA, COBLET_LOADER_REPLACE, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(
                add(FAKE_DATA, 4, &raw mut err),
                CobletStatus::InvalidArgument
            );
            take_error(err);
            let status = coblet_builder_add_loader_json(builder, c"{}".as_ptr(), 0, &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);

            let missing = CobletModLoader {
                install: None,
                ..counted()
            };
            let status = coblet_builder_add_loader(builder, missing, 0, &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
            assert_eq!(
                freed.load(Ordering::SeqCst),
                1,
                "a rejected table is freed at once"
            );
            assert_eq!(
                coblet_builder_add_loader(builder, counted(), 0, &raw mut err),
                CobletStatus::Ok
            );

            let status = coblet_builder_build(builder, &raw mut coblet, std::ptr::null_mut());
            assert_eq!(status, CobletStatus::Ok);
            assert_eq!(
                coblet_loaders_json(coblet, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            let ids: Vec<String> = take(&mut out)["loaders"]
                .as_array()
                .unwrap()
                .iter()
                .map(|loader| loader["id"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(ids, ["test:fake-c", "test:fake-data"]);
            coblet_free(coblet);
        }
        assert_eq!(freed.load(Ordering::SeqCst), 2);
    }
}
