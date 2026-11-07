use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use coblet::catalog::{
    CatalogContext, CatalogError, CatalogPage, CatalogQuery, CatalogSource, Timestamp,
};
use coblet::loader::{DataLoaderVersions, LoaderVersion};
use coblet::mojang::{GameVersion, VersionTrait, VersionTraits, fetch_for_source};
use coblet::runtime::{BoxFuture, Extension, HttpUrl};
use coblet::types::{ExtensionId, GameVersionId, Sha1Digest, VersionType};
use serde::Deserialize;

use crate::builder::{CobletBuilder, builder_mut};
use crate::context::Coblet;
use crate::customize::Owned;
use crate::guard::{guard, put};
use crate::types::{CobletBytes, CobletError, CobletStatus, CobletString, Failure, text};

/// Flag of `coblet_builder_add_version_source`,
/// `coblet_builder_add_loader_version_source` and the data forms: put the
/// source in place of the one with the same ID, keeping its place, instead
/// of failing.
pub const COBLET_SOURCE_REPLACE: u32 = 1;

/// Callback table of a version source written in C, for game versions
/// (`coblet_builder_add_version_source`) or for the versions of a Mod
/// Loader (`coblet_builder_add_loader_version_source`).
///
/// `id` and `name` are copied when the table is added. `list` runs on a
/// library worker thread where blocking is fine, once each time a list is
/// loaded or refreshed; for a Mod Loader, once per game version. It gets a
/// `CobletSourceCtx` handle, valid only during the call, and the query as
/// JSON: `{"schema":1,"game":null}` for game versions,
/// `{"schema":1,"game":"1.20.1"}` for Mod Loader versions.
///
/// It fetches documents with `coblet_source_ctx_fetch`, which goes through
/// the metadata cache and the mirror rules, and answers with
/// `coblet_source_ctx_set_result`: a JSON array of versions, newest first.
/// A game version is `{"id","type","url","sha1","released",
/// "latest_release","traits"}`: `url` and `sha1` name the version JSON in
/// Mojang's format, checked when the version is launched, `released` is
/// RFC 3339, `type` defaults to `release`, `latest_release` to false and
/// `traits` (declared version traits) to none. A Mod Loader version is an
/// item of `coblet_loader_versions_json`: `{"id","game","type","released",
/// "recommended","data"}`, where only `id` is required. Returning true
/// without an answer lists nothing. On failure it returns false,
/// optionally after `coblet_source_ctx_fail`; the other sources are still
/// listed and a `catalog.source-failed` warning is published. The library
/// owns `user_data` from the call that receives the table, even when it
/// fails, and calls `free` once when the `Coblet` is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletVersionSource {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:experiments`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// Lists the versions for the query in `query_json`. Returns false on failure.
    pub list: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletSourceCtx,
            query_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// What a source fetches for, which decides the cache folder and the kind
/// of request mirror rules see.
#[derive(Clone, Copy, Debug)]
enum Fetches {
    GameVersions,
    LoaderVersions,
}

/// One running version source callback: its requests and its answer.
pub struct CobletSourceCtx {
    catalog: CatalogContext,
    source: ExtensionId,
    fetches: Fetches,
    runtime: tokio::runtime::Handle,
    result: Option<String>,
    failure: Option<String>,
}

impl std::fmt::Debug for CobletSourceCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletSourceCtx")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

type Callback = unsafe extern "C" fn(*mut c_void, *mut CobletSourceCtx, *const c_char) -> bool;

/// C version source used as a coblet catalog source of either kind.
pub(crate) struct ForeignVersionSource {
    id: ExtensionId,
    name: String,
    callback: Callback,
    owned: Arc<Owned>,
}

impl ForeignVersionSource {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    unsafe fn new(table: &CobletVersionSource) -> Result<Self, Failure> {
        let owned = Arc::new(Owned {
            user_data: table.user_data,
            free: table.free,
        });
        let callback = table
            .list
            .ok_or_else(|| Failure::argument("version source", "has no callback"))?;
        // SAFETY: forwarded from the caller's guarantee.
        let (id, name) = unsafe { (text(table.id, "id")?, text(table.name, "name")?) };
        Ok(Self {
            id: source_id(id)?,
            name: name.to_owned(),
            callback,
            owned,
        })
    }

    /// Runs the callback on a blocking thread and returns its answer.
    async fn ask(
        &self,
        cx: &CatalogContext,
        fetches: Fetches,
        game: Option<&GameVersionId>,
    ) -> Result<Option<String>, CatalogError> {
        let query = serde_json::json!({ "schema": 1, "game": game.map(GameVersionId::as_str) });
        let argument = CString::new(query.to_string())
            .map_err(|_| CatalogError::Failed("query contains NUL".to_owned()))?;
        let mut ctx = CobletSourceCtx {
            catalog: CatalogContext::new(cx.meta().clone(), cx.policy()),
            source: self.id.clone(),
            fetches,
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
        .map_err(|error| self.failed(&format!("panicked: {error}")))?;
        for warning in ctx.catalog.take_warnings() {
            cx.warn(warning);
        }
        if !succeeded {
            let reason = ctx
                .failure
                .unwrap_or_else(|| "the callback failed".to_owned());
            return Err(self.failed(&reason));
        }
        Ok(ctx.result)
    }

    fn failed(&self, reason: &str) -> CatalogError {
        CatalogError::Failed(format!("version source `{}` failed: {reason}", self.id))
    }
}

impl Extension for ForeignVersionSource {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

#[allow(unsafe_code, reason = "calls a C version source callback")]
fn invoke(
    owned: &Owned,
    callback: Callback,
    ctx: &mut CobletSourceCtx,
    argument: &CString,
) -> bool {
    // SAFETY: the table owner promised a callback that accepts its user
    // data, the context handle and a NUL-terminated string, all valid for
    // the duration of the call.
    unsafe { callback(owned.user_data, ctx, argument.as_ptr()) }
}

/// One game version in the answer of a C game version source.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GameEntry {
    id: String,
    #[serde(rename = "type", default = "release")]
    version_type: String,
    url: String,
    sha1: String,
    released: String,
    #[serde(default)]
    latest_release: bool,
    #[serde(default)]
    traits: Vec<String>,
}

fn release() -> String {
    "release".to_owned()
}

impl GameEntry {
    fn into_version(self) -> Result<GameVersion, String> {
        let id = GameVersionId::new(&self.id).map_err(|error| format!("id: {error}"))?;
        let url = HttpUrl::new(&self.url).map_err(|error| format!("url: {error}"))?;
        let sha1 = Sha1Digest::from_hex(&self.sha1).map_err(|error| format!("sha1: {error}"))?;
        let released =
            Timestamp::parse(&self.released).map_err(|error| format!("released: {error}"))?;
        let traits = self
            .traits
            .into_iter()
            .map(|name| {
                if name.is_empty() || name.chars().any(char::is_whitespace) {
                    Err(format!("invalid trait name `{name}`"))
                } else {
                    Ok(VersionTrait::from(name))
                }
            })
            .collect::<Result<VersionTraits, _>>()?;
        let mut version = GameVersion::new(
            id,
            VersionType::from(self.version_type),
            url,
            sha1,
            released,
        )
        .with_traits(traits);
        version.latest_release = self.latest_release;
        Ok(version)
    }
}

impl CatalogSource<GameVersion> for ForeignVersionSource {
    fn fetch<'a>(
        &'a self,
        cx: &'a CatalogContext,
        _query: &'a CatalogQuery,
    ) -> BoxFuture<'a, Result<CatalogPage<GameVersion>, CatalogError>> {
        Box::pin(async move {
            let Some(json) = self.ask(cx, Fetches::GameVersions, None).await? else {
                return Ok(CatalogPage::new(Vec::new()));
            };
            let entries: Vec<GameEntry> = serde_json::from_str(&json)
                .map_err(|error| self.failed(&format!("answer: {error}")))?;
            let versions = entries
                .into_iter()
                .map(|entry| {
                    let id = entry.id.clone();
                    entry
                        .into_version()
                        .map_err(|error| self.failed(&format!("`{id}` has a bad {error}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CatalogPage::new(versions))
        })
    }
}

impl CatalogSource<LoaderVersion> for ForeignVersionSource {
    fn fetch<'a>(
        &'a self,
        cx: &'a CatalogContext,
        query: &'a CatalogQuery,
    ) -> BoxFuture<'a, Result<CatalogPage<LoaderVersion>, CatalogError>> {
        Box::pin(async move {
            let Some(game) = &query.game else {
                return Err(CatalogError::Failed(
                    "mod loader versions are listed per game version".to_owned(),
                ));
            };
            let Some(json) = self.ask(cx, Fetches::LoaderVersions, Some(game)).await? else {
                return Ok(CatalogPage::new(Vec::new()));
            };
            let mut versions: Vec<LoaderVersion> = serde_json::from_str(&json)
                .map_err(|error| self.failed(&format!("answer: {error}")))?;
            for version in &mut versions {
                version.game.get_or_insert_with(|| game.clone());
            }
            Ok(CatalogPage::new(versions))
        })
    }
}

fn source_id(id: &str) -> Result<ExtensionId, Failure> {
    ExtensionId::new(id).map_err(|error| Failure::argument("version source id", &error.to_string()))
}

fn loader_id(id: &str) -> Result<ExtensionId, Failure> {
    ExtensionId::new(id).map_err(|error| Failure::argument("loader id", &error.to_string()))
}

/// Reads a context handle argument.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(ctx: *mut CobletSourceCtx) -> Result<&'a mut CobletSourceCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Fetches a document for the running version source callback and caches
/// it under `cache_name`, a relative path such as `versions.json`; blocks
/// until it is there.
///
/// The request goes through the metadata cache, so mirror rules and
/// fallback apply. A game version source's documents are cached under
/// `sources/<namespace>/<name>/`, a Mod Loader version source's as Mod
/// Loader metadata under `loaders/<namespace>/<name>/`. On success
/// `*out_body` receives bytes to free with `coblet_bytes_free`. An address
/// that is not HTTP or HTTPS or an unsafe cache name fails, a document that
/// is unavailable and not cached with `COBLET_ERROR_KIND_NETWORK`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; strings must be NULL or NUL-terminated; `out_body`
/// must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_source_ctx_fetch(
    ctx: *mut CobletSourceCtx,
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
            // The callback runs on a blocking thread, outside the runtime's
            // own tasks, so waiting here is allowed.
            let body = ctx.runtime.block_on(async {
                Ok::<_, coblet::Error>(match ctx.fetches {
                    Fetches::GameVersions => {
                        fetch_for_source(&ctx.catalog, &ctx.source, url, name).await?
                    }
                    Fetches::LoaderVersions => {
                        coblet::loader::LoaderContext::new(&ctx.catalog, &ctx.source)
                            .fetch(url, name)
                            .await?
                    }
                })
            })?;
            put(out_body, CobletBytes::new(body.to_vec()), "out_body")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Hands the answer of the running version source callback to the
/// library, which copies it: a JSON array of versions.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `json` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_source_ctx_set_result(
    ctx: *mut CobletSourceCtx,
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

/// Gives the reason the running version source callback is about to
/// return false.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_source_ctx_fail(
    ctx: *mut CobletSourceCtx,
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

type GameSource = Arc<dyn CatalogSource<GameVersion>>;
type LoaderSource = Arc<dyn CatalogSource<LoaderVersion>>;

/// One game or Mod Loader version source change, applied in order.
pub(crate) enum SourceChange {
    Game(GameSource, bool),
    RemoveGame(ExtensionId),
    Loader(ExtensionId, LoaderSource, bool),
    RemoveLoader(ExtensionId, ExtensionId),
}

impl SourceChange {
    /// Adds a game version source without replacing one.
    pub(crate) fn game(source: GameSource) -> Self {
        Self::Game(source, false)
    }

    /// Applies the change to the engine builder.
    pub(crate) fn apply(
        self,
        builder: coblet::CobletBuilder<coblet::WithRoot>,
    ) -> coblet::CobletBuilder<coblet::WithRoot> {
        match self {
            Self::Game(source, false) => builder.version_source(source),
            Self::Game(source, true) => builder.replace_version_source(source),
            Self::RemoveGame(id) => builder.without_version_source(id),
            Self::Loader(loader, source, false) => builder.loader_version_source(loader, source),
            Self::Loader(loader, source, true) => {
                builder.replace_loader_version_source(loader, source)
            }
            Self::RemoveLoader(loader, id) => builder.without_loader_version_source(loader, id),
        }
    }
}

/// Reads the flags of the source registration calls.
fn replace(flags: u32) -> Result<bool, Failure> {
    if flags & !COBLET_SOURCE_REPLACE != 0 {
        return Err(Failure::argument(
            "flags",
            &format!("unknown flags {flags:#x}"),
        ));
    }
    Ok(flags & COBLET_SOURCE_REPLACE != 0)
}

/// Adds a source change to a builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder handle; `out_err` must be NULL
/// or writable.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn push(
    builder: *mut CobletBuilder,
    out_err: *mut *mut CobletError,
    change: impl FnOnce() -> Result<SourceChange, Failure>,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            builder.version_sources.push(change()?);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a game version source written in C after the Mojang manifest and
/// the sources already added; when two list a version ID, the earlier one
/// wins.
///
/// `flags` is 0 or `COBLET_SOURCE_REPLACE`, which puts the source in place
/// of the one with the same ID, such as `coblet:mojang`. The library owns
/// the table's `user_data` from this call on, even when it fails. A table
/// without its callback, a malformed ID or an unknown flag fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`; an ID already used without the flag
/// fails `coblet_builder_build`.
///
/// # Safety
///
/// `builder` must be a live builder handle; the table's strings must be
/// NULL or NUL-terminated and its callbacks must stay valid until its
/// `free` runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_version_source(
    builder: *mut CobletBuilder,
    source: CobletVersionSource,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignVersionSource::new(&source) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let replace = replace(flags)?;
            Ok(SourceChange::Game(Arc::new(adapter?), replace))
        })
    }
}

/// Removes a game version source, such as `coblet:mojang` for a launcher
/// that lists only its own versions; an unknown ID is ignored. Removing
/// every source makes `coblet_builder_build` fail.
///
/// # Safety
///
/// `builder` must be a live builder handle; `id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_version_source(
    builder: *mut CobletBuilder,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            Ok(SourceChange::RemoveGame(source_id(text(id, "id")?)?))
        })
    }
}

/// Adds a version source written in C to the Mod Loader `loader`, asked
/// after the loader itself and the sources already added; for the same
/// version string the earlier entry wins.
///
/// `flags` is as for `coblet_builder_add_version_source`. The library owns
/// the table's `user_data` from this call on, even when it fails. A table
/// without its callback, a malformed ID or an unknown flag fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`; an unknown loader, or an ID the
/// loader's sources already use, fails `coblet_builder_build`.
///
/// # Safety
///
/// As `coblet_builder_add_version_source`; `loader` must be NULL or
/// NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_loader_version_source(
    builder: *mut CobletBuilder,
    loader: *const c_char,
    source: CobletVersionSource,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignVersionSource::new(&source) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let loader = loader_id(text(loader, "loader")?)?;
            let replace = replace(flags)?;
            Ok(SourceChange::Loader(loader, Arc::new(adapter?), replace))
        })
    }
}

/// Adds a version source described in TOML to the Mod Loader `loader`: an
/// `id`, a `name` and a `[versions]` table in the format of data loaders
/// (`coblet_builder_add_loader_toml`), such as a mirror's listing. `flags`
/// is as for `coblet_builder_add_version_source`. A malformed definition
/// fails with `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_loader_version_source_toml(
    builder: *mut CobletBuilder,
    loader: *const c_char,
    definition: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let loader = loader_id(text(loader, "loader")?)?;
            let replace = replace(flags)?;
            let source = DataLoaderVersions::from_toml(text(definition, "definition")?)
                .map_err(|error| Failure::argument("definition", &error.to_string()))?;
            Ok(SourceChange::Loader(loader, Arc::new(source), replace))
        })
    }
}

/// Adds a version source described in JSON to the Mod Loader `loader`,
/// with the keys of the TOML form of
/// `coblet_builder_add_loader_version_source_toml`.
///
/// # Safety
///
/// As `coblet_builder_add_loader_version_source_toml`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_loader_version_source_json(
    builder: *mut CobletBuilder,
    loader: *const c_char,
    definition: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let loader = loader_id(text(loader, "loader")?)?;
            let replace = replace(flags)?;
            let source = DataLoaderVersions::from_json(text(definition, "definition")?)
                .map_err(|error| Failure::argument("definition", &error.to_string()))?;
            Ok(SourceChange::Loader(loader, Arc::new(source), replace))
        })
    }
}

/// Removes a version source of the Mod Loader `loader`; an unknown ID is
/// ignored.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_loader_version_source(
    builder: *mut CobletBuilder,
    loader: *const c_char,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let loader = loader_id(text(loader, "loader")?)?;
            Ok(SourceChange::RemoveLoader(
                loader,
                source_id(text(id, "id")?)?,
            ))
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

fn sources_json<'a, Item: 'static>(
    sources: impl Iterator<Item = &'a Arc<dyn CatalogSource<Item>>>,
) -> String {
    let sources: Vec<_> = sources
        .map(|source| {
            serde_json::json!({ "id": source.id().as_str(), "name": source.display_name() })
        })
        .collect();
    serde_json::json!({ "schema": 1, "sources": sources }).to_string()
}

/// Lists the game version sources, in the order they are asked, as JSON:
/// `{"schema":1,"sources":[{"id","name"}]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_version_sources_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let json = sources_json(handle(coblet)?.inner.version_sources().iter());
            put(out_json, CobletString::new(&json), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Lists the extra version sources of the Mod Loader `loader`, in the
/// order they are asked after the loader itself, as JSON:
/// `{"schema":1,"sources":[{"id","name"}]}`. An unknown loader fails with
/// `COBLET_ERROR_KIND_NOT_FOUND`. On success `*out_json` receives a string
/// to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `loader` must be NULL or
/// NUL-terminated; `out_json` must be writable; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_loader_version_sources_json(
    coblet: *const Coblet,
    loader: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let loaders = handle(coblet)?.inner.loaders();
            let id = loader_id(text(loader, "loader")?)?;
            if loaders.get(&id).is_none() {
                return Err(
                    coblet::Error::from(coblet::loader::LoaderError::UnknownLoader(id)).into(),
                );
            }
            let json = match loaders.version_sources(&id) {
                Some(sources) => sources_json(sources.iter()),
                None => sources_json(std::iter::empty::<&Arc<dyn CatalogSource<LoaderVersion>>>()),
            };
            put(out_json, CobletString::new(&json), "out_json")?;
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
    use coblet::types::{Arch, ExtensionId, OsFamily, Platform};

    use super::{
        COBLET_SOURCE_REPLACE, CobletSourceCtx, CobletVersionSource, ForeignVersionSource,
        coblet_builder_add_loader_version_source, coblet_builder_add_loader_version_source_json,
        coblet_builder_add_loader_version_source_toml, coblet_builder_add_version_source,
        coblet_builder_remove_loader_version_source, coblet_builder_remove_version_source,
        coblet_loader_version_sources_json, coblet_source_ctx_fail, coblet_source_ctx_fetch,
        coblet_source_ctx_set_result, coblet_version_sources_json,
    };
    use crate::accounts::coblet_bytes_free;
    use crate::builder::{
        coblet_builder_add_version_manifest, coblet_builder_build, coblet_builder_new,
    };
    use crate::context::{
        Coblet, coblet_error_free, coblet_free, coblet_string_free, coblet_versions_json,
    };
    use crate::loaders::{coblet_builder_add_loader_toml, coblet_loader_versions_json};
    use crate::traits::coblet_version_traits_json;
    use crate::types::{CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString};

    /// Serves the Mojang fixtures by file name from any host.
    struct Fixtures;

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
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

    fn answer(ctx: *mut CobletSourceCtx, json: &serde_json::Value) -> bool {
        let json = CString::new(json.to_string()).unwrap();
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_source_ctx_set_result(ctx, json.as_ptr()) == CobletStatus::Ok }
    }

    /// Lists b1.7.3 from its own host with the SHA-1 its manifest gives.
    unsafe extern "C" fn own(
        _user_data: *mut c_void,
        ctx: *mut CobletSourceCtx,
        query: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated JSON.
        let query = unsafe { CStr::from_ptr(query) }.to_str().unwrap();
        assert_eq!(query, r#"{"game":null,"schema":1}"#);
        let mut body = CobletBytes::empty();
        let mut err = std::ptr::null_mut();
        // SAFETY: the running callback's handle, valid strings and out-pointers.
        let status = unsafe {
            coblet_source_ctx_fetch(
                ctx,
                c"https://own.fake.test/version_manifest_v2.json".as_ptr(),
                c"manifest.json".as_ptr(),
                &raw mut body,
                &raw mut err,
            )
        };
        assert_eq!(status, CobletStatus::Ok);
        // SAFETY: the bytes were just returned and are freed once.
        let manifest: serde_json::Value = unsafe {
            let value = serde_json::from_slice(std::slice::from_raw_parts(body.ptr, body.len));
            coblet_bytes_free(body);
            value.unwrap()
        };
        let beta = manifest["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == "b1.7.3")
            .unwrap();
        answer(
            ctx,
            &serde_json::json!([{
                "id": "b1.7.3",
                "type": "old_beta",
                "url": "https://own.fake.test/b1.7.3.json",
                "sha1": beta["sha1"],
                "released": beta["releaseTime"],
                "latest_release": true,
                "traits": ["acme:declared"]
            }]),
        )
    }

    unsafe extern "C" fn nightly(
        _user_data: *mut c_void,
        ctx: *mut CobletSourceCtx,
        query: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated JSON.
        let query = unsafe { CStr::from_ptr(query) }.to_str().unwrap();
        assert_eq!(query, r#"{"game":"1.20.1","schema":1}"#);
        answer(
            ctx,
            &serde_json::json!([{ "id": "2.0", "type": "beta" }, { "id": "1.0" }]),
        )
    }

    unsafe extern "C" fn down(
        _user_data: *mut c_void,
        ctx: *mut CobletSourceCtx,
        _query: *const c_char,
    ) -> bool {
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_source_ctx_fail(ctx, c"offline".as_ptr()) };
        false
    }

    fn table(
        id: &'static CStr,
        list: super::Callback,
        user_data: *mut c_void,
    ) -> CobletVersionSource {
        CobletVersionSource {
            user_data,
            id: id.as_ptr(),
            name: c"Test source".as_ptr(),
            list: Some(list),
            free: Some(free),
        }
    }

    const LOADER: &CStr = c"id = \"test:fake\"\nname = \"Fake\"\nuid = \"test.fake\"\n\
        [versions]\nformat = \"static\"\nlist = [{ id = \"1.0\" }]\n\
        [profile]\nformat = \"static\"\n[[profile.components]]\nuid = \"test.fake\"\nversion = \"{version}\"\n";

    fn take(out: &mut CobletString) -> serde_json::Value {
        let value = std::mem::replace(out, CobletString::empty());
        // SAFETY: the string was just returned and is freed once.
        unsafe {
            let text = CStr::from_ptr(value.ptr).to_str().unwrap().to_owned();
            coblet_string_free(value);
            serde_json::from_str(&text).unwrap()
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

    /// A handle with the fixtures transport and the C sources.
    fn coblet(root: &std::path::Path) -> *mut Coblet {
        // SAFETY: the tables hold static strings.
        let (own, nightly, down) = unsafe {
            (
                ForeignVersionSource::new(&table(c"test:own", own, std::ptr::null_mut())).unwrap(),
                ForeignVersionSource::new(&table(c"test:nightly", nightly, std::ptr::null_mut()))
                    .unwrap(),
                ForeignVersionSource::new(&table(c"test:down", down, std::ptr::null_mut()))
                    .unwrap(),
            )
        };
        let loader = ExtensionId::new("test:fake").unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let inner = runtime.block_on(async {
            coblet::Coblet::builder()
                .data_root(root)
                .transport(Arc::new(Fixtures))
                .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
                .without_version_source(ExtensionId::new("coblet:mojang").unwrap())
                .version_source(Arc::new(own))
                .loader_toml(LOADER.to_str().unwrap())
                .loader_version_source(loader.clone(), Arc::new(nightly))
                .loader_version_source(loader, Arc::new(down))
                .build()
                .unwrap()
        });
        Box::into_raw(Box::new(Coblet { runtime, inner }))
    }

    #[test]
    fn c_sources_list_game_and_loader_versions() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(dir.path());
        let mut out = CobletString::empty();
        let mut err = std::ptr::null_mut();
        // SAFETY: a live handle, valid strings and out-pointers.
        unsafe {
            assert_eq!(
                coblet_version_sources_json(coblet, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(take(&mut out)["sources"][0]["id"], "test:own");

            assert_eq!(
                coblet_versions_json(coblet, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            let versions = take(&mut out);
            let listed = versions["versions"].as_array().unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0]["source"], "test:own");
            assert_eq!(listed[0]["latest_release"], true);
            assert!(
                listed[0]["traits"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("acme:declared"))
            );
            assert!(
                dir.path()
                    .join("meta/sources/test/own/manifest.json")
                    .is_file()
            );
            // The version JSON comes from the entry's address, checked by its SHA-1.
            assert_eq!(
                coblet_version_traits_json(coblet, c"b1.7.3".as_ptr(), &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            assert!(
                take(&mut out)["traits"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("jre-legacy"))
            );

            assert_eq!(
                coblet_loader_versions_json(
                    coblet,
                    c"test:fake".as_ptr(),
                    c"1.20.1".as_ptr(),
                    &raw mut out,
                    &raw mut err
                ),
                CobletStatus::Ok
            );
            let merged: Vec<(String, String)> = take(&mut out)["versions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|version| {
                    (
                        version["id"].as_str().unwrap().to_owned(),
                        version["source"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            assert_eq!(
                merged,
                [
                    ("1.0".to_owned(), "test:fake".to_owned()),
                    ("2.0".to_owned(), "test:nightly".to_owned())
                ]
            );
            assert_eq!(
                coblet_loader_version_sources_json(
                    coblet,
                    c"test:fake".as_ptr(),
                    &raw mut out,
                    &raw mut err
                ),
                CobletStatus::Ok
            );
            let ids: Vec<_> = take(&mut out)["sources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|source| source["id"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(ids, ["test:nightly", "test:down"]);
            assert_ne!(
                coblet_loader_version_sources_json(
                    coblet,
                    c"test:missing".as_ptr(),
                    &raw mut out,
                    &raw mut err
                ),
                CobletStatus::Ok
            );
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
    fn the_builder_checks_flags_ids_and_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let frees = AtomicUsize::new(0);
        let counter = std::ptr::from_ref(&frees).cast_mut().cast::<c_void>();
        let mut err = std::ptr::null_mut();
        let mut coblet = std::ptr::null_mut();
        // SAFETY: live builders, tables with static strings, valid out-pointers.
        unsafe {
            let builder = new_builder(dir.path());
            let status = coblet_builder_add_version_source(
                builder,
                table(c"test:own", own, counter),
                4,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
            let unnamed = CobletVersionSource {
                list: None,
                ..table(c"test:own", own, counter)
            };
            let status = coblet_builder_add_loader_version_source(
                builder,
                c"test:fake".as_ptr(),
                unnamed,
                0,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
            // Both rejected tables were freed at once.
            assert_eq!(frees.load(Ordering::SeqCst), 2);

            let calls: [&dyn Fn(*mut *mut CobletError) -> CobletStatus; 4] = [
                &|err| {
                    coblet_builder_add_loader_version_source_toml(
                        builder,
                        c"test:fake".as_ptr(),
                        c"id = 1".as_ptr(),
                        0,
                        err,
                    )
                },
                &|err| {
                    coblet_builder_add_loader_version_source_json(
                        builder,
                        c"no namespace".as_ptr(),
                        c"{}".as_ptr(),
                        0,
                        err,
                    )
                },
                &|err| {
                    coblet_builder_remove_loader_version_source(
                        builder,
                        c"test:fake".as_ptr(),
                        c"bad id".as_ptr(),
                        err,
                    )
                },
                &|err| coblet_builder_remove_version_source(builder, c"bad id".as_ptr(), err),
            ];
            for call in calls {
                assert_eq!(call(&raw mut err), CobletStatus::InvalidArgument);
                take_error(err);
            }
            // Two sources with one ID fail the build.
            let url = c"https://meta.example/experiments.json";
            for _ in 0..2 {
                let status = coblet_builder_add_version_manifest(
                    builder,
                    c"acme:experiments".as_ptr(),
                    url.as_ptr(),
                    &raw mut err,
                );
                assert_eq!(status, CobletStatus::Ok);
            }
            let status = coblet_builder_build(builder, &raw mut coblet, &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
        }
    }

    #[test]
    fn loader_sources_need_their_loader() {
        let dir = tempfile::tempdir().unwrap();
        let frees = AtomicUsize::new(0);
        let counter = std::ptr::from_ref(&frees).cast_mut().cast::<c_void>();
        let mut err = std::ptr::null_mut();
        let mut coblet = std::ptr::null_mut();
        // SAFETY: live builders, tables with static strings, valid out-pointers.
        unsafe {
            // A source for a loader nobody registered fails the build.
            let builder = new_builder(dir.path());
            let status = coblet_builder_add_loader_version_source(
                builder,
                c"test:fake".as_ptr(),
                table(c"test:nightly", nightly, counter),
                COBLET_SOURCE_REPLACE,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            let status = coblet_builder_build(builder, &raw mut coblet, &raw mut err);
            assert_ne!(status, CobletStatus::Ok);
            assert_eq!(take_error(err), CobletErrorKind::NotFound);
            assert_eq!(frees.load(Ordering::SeqCst), 1);

            let builder = new_builder(dir.path());
            assert_eq!(
                coblet_builder_add_loader_toml(builder, LOADER.as_ptr(), 0, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_add_loader_version_source_toml(
                    builder,
                    c"test:fake".as_ptr(),
                    c"id = \"test:fixed\"\nname = \"Fixed\"\n[versions]\nformat = \"static\"\nlist = []".as_ptr(),
                    0,
                    &raw mut err,
                ),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_build(builder, &raw mut coblet, &raw mut err),
                CobletStatus::Ok
            );
            coblet_free(coblet);
        }
    }
}
