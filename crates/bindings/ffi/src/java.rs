use std::ffi::{CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::Arc;

use coblet::catalog::CatalogContext;
use coblet::java::{
    DataJavaProvider, InstallSource, JavaCandidate, JavaError, JavaInstall, JavaPackage, JavaQuery,
    LocateContext, ProviderContext,
};
use coblet::runtime::{BoxFuture, CancellationToken, Extension, OperationId};
use coblet::types::ExtensionId;

use crate::builder::{CobletBuilder, builder_mut};
use crate::context::Coblet;
use crate::customize::Owned;
use crate::game::operation_id;
use crate::guard::{guard, put};
use crate::types::{CobletBytes, CobletError, CobletStatus, CobletString, Failure, text};

/// Flag of `coblet_builder_add_java_provider`, `coblet_builder_add_java_locator`
/// and the data forms: put the extension in place of the one with the same
/// ID, keeping its place, instead of failing.
pub const COBLET_JAVA_REPLACE: u32 = 1;

/// Callback table of a Java provider written in C.
///
/// `id` and `name` are copied when the table is added. `packages` runs on a
/// library worker thread where blocking is fine and gets a `CobletJavaCtx`
/// handle, valid only during the call, and the query as JSON:
/// `{"os","arch","major","component","runtime_platform"}`, the last two
/// optional. It fetches documents with `coblet_java_ctx_fetch`, which goes
/// through the metadata cache and the mirror rules, answers with
/// `coblet_java_ctx_set_result` and returns true, or gives a reason with
/// `coblet_java_ctx_fail` and returns false.
///
/// The answer is a JSON array of packages, each `{"id","version","major",
/// "arch","download"}` where `download` is `{"type":"archive","url","hash",
/// "format","size","home"}` with `format` `zip` or `tar.gz`, or
/// `{"type":"manifest","url","hash","size"}` for a Mojang-style file list;
/// `size` and `home` are optional. Every package carries a hash such as
/// `sha256:…`: the engine downloads, checks and unpacks; providers only
/// describe. The library owns `user_data` from the call that receives the
/// table, even when it fails, and calls `free` once when the `Coblet` is
/// freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletJavaProvider {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:jdk`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// Lists the packages that fit `query_json`. Returns false on failure.
    pub packages: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletJavaCtx,
            query_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// Callback table of a place to look for installed Java, written in C.
///
/// `id` and `name` are copied when the table is added. `locate` runs on a
/// library worker thread where blocking is fine, gets the operating system
/// such as `macos`, and answers with `coblet_java_ctx_set_result` and a
/// JSON array of `java` executable paths, best first; it returns false,
/// optionally after `coblet_java_ctx_fail`, when it cannot look. The
/// library runs each executable to read its version. `coblet_java_ctx_fetch`
/// is not available to locators. Ownership of `user_data` is as for
/// `CobletJavaProvider`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletJavaLocator {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:other-launcher`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// Lists executables for the operating system `os`. Returns false on failure.
    pub locate: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletJavaCtx,
            os: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// One running Java provider or locator callback: its requests and its answer.
pub struct CobletJavaCtx {
    /// Metadata access, for providers only.
    catalog: Option<CatalogContext>,
    extension: ExtensionId,
    runtime: tokio::runtime::Handle,
    result: Option<String>,
    failure: Option<String>,
}

impl std::fmt::Debug for CobletJavaCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletJavaCtx")
            .field("extension", &self.extension)
            .finish_non_exhaustive()
    }
}

type Callback = unsafe extern "C" fn(*mut c_void, *mut CobletJavaCtx, *const c_char) -> bool;

/// A C callback table taken over by the library.
struct Table {
    id: ExtensionId,
    name: String,
    callback: Callback,
    owned: Arc<Owned>,
}

impl Table {
    /// Takes over the parts shared by both tables.
    ///
    /// # Safety
    ///
    /// `id` and `name` must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    unsafe fn new(
        what: &str,
        owned: Owned,
        id: *const c_char,
        name: *const c_char,
        callback: Option<Callback>,
    ) -> Result<Self, Failure> {
        let owned = Arc::new(owned);
        let callback = callback.ok_or_else(|| Failure::argument(what, "has no callback"))?;
        // SAFETY: forwarded from the caller's guarantee.
        let (id, name) = unsafe { (text(id, "id")?, text(name, "name")?) };
        Ok(Self {
            id: extension_id(what, id)?,
            name: name.to_owned(),
            callback,
            owned,
        })
    }

    /// Runs the callback on a blocking thread and returns its answer.
    async fn call(
        &self,
        catalog: Option<CatalogContext>,
        argument: String,
    ) -> Result<(String, CobletJavaCtx), String> {
        let mut ctx = CobletJavaCtx {
            catalog,
            extension: self.id.clone(),
            runtime: tokio::runtime::Handle::current(),
            result: None,
            failure: None,
        };
        let argument = CString::new(argument).map_err(|_| "argument contains NUL".to_owned())?;
        let (owned, callback) = (Arc::clone(&self.owned), self.callback);
        let (succeeded, ctx) = tokio::task::spawn_blocking(move || {
            let succeeded = invoke(&owned, callback, &mut ctx, &argument);
            (succeeded, ctx)
        })
        .await
        .map_err(|error| format!("the callback panicked: {error}"))?;
        match (succeeded, ctx.result.clone()) {
            (true, Some(result)) => Ok((result, ctx)),
            (true, None) => Err("the callback set no result".to_owned()),
            (false, _) => Err(ctx
                .failure
                .clone()
                .unwrap_or_else(|| "the callback failed".to_owned())),
        }
    }
}

#[allow(unsafe_code, reason = "calls a C Java callback")]
fn invoke(owned: &Owned, callback: Callback, ctx: &mut CobletJavaCtx, argument: &CString) -> bool {
    // SAFETY: the table owner promised a callback that accepts its user
    // data, the context handle and a NUL-terminated string, all valid for
    // the duration of the call.
    unsafe { callback(owned.user_data, ctx, argument.as_ptr()) }
}

fn extension_id(what: &str, id: &str) -> Result<ExtensionId, Failure> {
    ExtensionId::new(id)
        .map_err(|error| Failure::argument(&format!("{what} id"), &error.to_string()))
}

/// C Java provider used as a coblet Java provider.
pub(crate) struct ForeignProvider(Table);

impl ForeignProvider {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    unsafe fn new(table: &CobletJavaProvider) -> Result<Self, Failure> {
        let owned = Owned {
            user_data: table.user_data,
            free: table.free,
        };
        // SAFETY: forwarded from the caller's guarantee.
        let table =
            unsafe { Table::new("java provider", owned, table.id, table.name, table.packages) };
        table.map(Self)
    }
}

impl Extension for ForeignProvider {
    fn id(&self) -> &ExtensionId {
        &self.0.id
    }

    fn display_name(&self) -> &str {
        &self.0.name
    }
}

impl coblet::java::JavaProvider for ForeignProvider {
    fn packages<'a>(
        &'a self,
        cx: &'a ProviderContext<'a>,
        query: &'a JavaQuery,
    ) -> BoxFuture<'a, Result<Vec<JavaPackage>, JavaError>> {
        Box::pin(async move {
            let id = &self.0.id;
            let query = serde_json::to_string(query)
                .map_err(|error| JavaError::provider(id, error.to_string()))?;
            let catalog = cx.catalog();
            let own = CatalogContext::new(catalog.meta().clone(), catalog.policy());
            let answer = self.0.call(Some(own), query).await;
            let (json, ctx) = answer.map_err(|reason| JavaError::provider(id, reason))?;
            if let Some(own) = &ctx.catalog {
                for warning in own.take_warnings() {
                    catalog.warn(warning);
                }
            }
            serde_json::from_str(&json)
                .map_err(|error| JavaError::provider(id, format!("packages: {error}")))
        })
    }
}

/// C Java locator used as a coblet Java locator.
pub(crate) struct ForeignLocator(Table);

impl ForeignLocator {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    unsafe fn new(table: &CobletJavaLocator) -> Result<Self, Failure> {
        let owned = Owned {
            user_data: table.user_data,
            free: table.free,
        };
        // SAFETY: forwarded from the caller's guarantee.
        let table =
            unsafe { Table::new("java locator", owned, table.id, table.name, table.locate) };
        table.map(Self)
    }
}

impl Extension for ForeignLocator {
    fn id(&self) -> &ExtensionId {
        &self.0.id
    }

    fn display_name(&self) -> &str {
        &self.0.name
    }
}

impl coblet::java::JavaLocator for ForeignLocator {
    fn locate<'a>(&'a self, cx: &'a LocateContext<'a>) -> BoxFuture<'a, Vec<JavaCandidate>> {
        Box::pin(async move {
            let id = &self.0.id;
            let paths = match self.0.call(None, cx.os.as_str().to_owned()).await {
                Ok((json, _)) => serde_json::from_str::<Vec<PathBuf>>(&json)
                    .map_err(|error| format!("executables: {error}")),
                Err(reason) => Err(reason),
            };
            match paths {
                Ok(paths) => paths
                    .into_iter()
                    .map(|executable| {
                        let source = InstallSource::Located {
                            locator: id.clone(),
                        };
                        JavaCandidate::new(executable, source)
                    })
                    .collect(),
                Err(reason) => {
                    tracing::warn!(locator = %id, %reason, "java locator failed");
                    Vec::new()
                }
            }
        })
    }
}

/// Reads a context handle argument.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(ctx: *mut CobletJavaCtx) -> Result<&'a mut CobletJavaCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Fetches a document for the running Java provider callback and caches it
/// under `cache_name`, a relative path such as `17/macos-aarch64.json`;
/// blocks until it is there.
///
/// The request goes through the metadata cache, so mirror rules and
/// fallback apply. On success `*out_body` receives bytes to free with
/// `coblet_bytes_free`. A locator's context, an address that is not HTTP
/// or HTTPS or an unsafe cache name fails, a document that is unavailable
/// and not cached with `COBLET_ERROR_KIND_NETWORK`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; strings must be NULL or NUL-terminated; `out_body`
/// must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_java_ctx_fetch(
    ctx: *mut CobletJavaCtx,
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
            let catalog = ctx.catalog.as_ref().ok_or_else(|| {
                Failure::argument("ctx", "belongs to a locator, which cannot fetch")
            })?;
            let cx = ProviderContext::new(catalog, &ctx.extension);
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

/// Hands the answer of the running Java callback to the library, which
/// copies it: the JSON array of packages or of executables.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `json` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_java_ctx_set_result(
    ctx: *mut CobletJavaCtx,
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

/// Gives the reason the running Java callback is about to return false.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_java_ctx_fail(
    ctx: *mut CobletJavaCtx,
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

/// One Java change, applied in order on top of the built-in lists.
pub(crate) enum JavaChange {
    Provider(Arc<dyn coblet::java::JavaProvider>, bool),
    RemoveProvider(ExtensionId),
    Locator(Arc<dyn coblet::java::JavaLocator>, bool),
    RemoveLocator(ExtensionId),
    Executable(String),
    Directory(String, String),
}

impl JavaChange {
    /// Applies the change to the engine builder.
    pub(crate) fn apply(
        self,
        builder: coblet::CobletBuilder<coblet::WithRoot>,
    ) -> coblet::CobletBuilder<coblet::WithRoot> {
        match self {
            Self::Provider(provider, false) => builder.java_provider(provider),
            Self::Provider(provider, true) => builder.replace_java_provider(provider),
            Self::RemoveProvider(id) => builder.without_java_provider(id),
            Self::Locator(locator, false) => builder.java_locator(locator),
            Self::Locator(locator, true) => builder.replace_java_locator(locator),
            Self::RemoveLocator(id) => builder.without_java_locator(id),
            Self::Executable(path) => builder.java_executable(path),
            Self::Directory(dir, home) => builder.java_directory(dir, home),
        }
    }
}

/// Reads the flags of the Java registration calls.
fn replace(flags: u32) -> Result<bool, Failure> {
    if flags & !COBLET_JAVA_REPLACE != 0 {
        return Err(Failure::argument(
            "flags",
            &format!("unknown flags {flags:#x}"),
        ));
    }
    Ok(flags & COBLET_JAVA_REPLACE != 0)
}

/// Adds a Java change to a builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder handle; `out_err` must be NULL
/// or writable.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn push(
    builder: *mut CobletBuilder,
    out_err: *mut *mut CobletError,
    change: impl FnOnce() -> Result<JavaChange, Failure>,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            builder.java.push(change()?);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a Java provider written in C, asked after the built-in ones
/// (Mojang, Adoptium, Azul Zulu) when no installed Java fits.
///
/// `flags` is 0 or `COBLET_JAVA_REPLACE`. The library owns the table's
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
pub unsafe extern "C" fn coblet_builder_add_java_provider(
    builder: *mut CobletBuilder,
    provider: CobletJavaProvider,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignProvider::new(&provider) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let replace = replace(flags)?;
            Ok(JavaChange::Provider(Arc::new(adapter?), replace))
        })
    }
}

/// Adds a Java provider described in TOML, in the format of coblet's data
/// Java providers. `flags` is as for `coblet_builder_add_java_provider`; a
/// malformed definition fails with `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `definition` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_java_provider_toml(
    builder: *mut CobletBuilder,
    definition: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let replace = replace(flags)?;
            let provider = DataJavaProvider::from_toml(text(definition, "definition")?)
                .map_err(|error| Failure::argument("definition", &error.to_string()))?;
            Ok(JavaChange::Provider(Arc::new(provider), replace))
        })
    }
}

/// Adds a Java provider described in JSON, with the keys of the TOML form
/// of `coblet_builder_add_java_provider_toml`.
///
/// # Safety
///
/// As `coblet_builder_add_java_provider_toml`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_java_provider_json(
    builder: *mut CobletBuilder,
    definition: *const c_char,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let replace = replace(flags)?;
            let provider = DataJavaProvider::from_json(text(definition, "definition")?)
                .map_err(|error| Failure::argument("definition", &error.to_string()))?;
            Ok(JavaChange::Provider(Arc::new(provider), replace))
        })
    }
}

/// Removes a Java provider, such as `coblet:mojang`; an unknown ID is ignored.
///
/// # Safety
///
/// `builder` must be a live builder handle; `id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_java_provider(
    builder: *mut CobletBuilder,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let id = extension_id("java provider", text(id, "id")?)?;
            Ok(JavaChange::RemoveProvider(id))
        })
    }
}

/// Adds a place to look for installed Java, written in C, searched after
/// the built-in places. `flags` and ownership are as for
/// `coblet_builder_add_java_provider`.
///
/// # Safety
///
/// As `coblet_builder_add_java_provider`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_java_locator(
    builder: *mut CobletBuilder,
    locator: CobletJavaLocator,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignLocator::new(&locator) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            let replace = replace(flags)?;
            Ok(JavaChange::Locator(Arc::new(adapter?), replace))
        })
    }
}

/// Removes a Java locator, such as `coblet:path` to ignore `PATH`; an
/// unknown ID is ignored.
///
/// # Safety
///
/// As `coblet_builder_remove_java_provider`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_java_locator(
    builder: *mut CobletBuilder,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let id = extension_id("java locator", text(id, "id")?)?;
            Ok(JavaChange::RemoveLocator(id))
        })
    }
}

/// Adds a `java` executable chosen by the user, searched before every other place.
///
/// # Safety
///
/// `builder` must be a live builder handle; `executable` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_java_executable(
    builder: *mut CobletBuilder,
    executable: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            Ok(JavaChange::Executable(
                text(executable, "executable")?.to_owned(),
            ))
        })
    }
}

/// Adds a folder holding one Java per child, such as another launcher's
/// runtime folder; `home` leads from each child to its Java home, like
/// `Contents/Home` on macOS or `""`.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_java_directory(
    builder: *mut CobletBuilder,
    dir: *const c_char,
    home: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let (dir, home) = (text(dir, "dir")?, text(home, "home")?);
            Ok(JavaChange::Directory(dir.to_owned(), home.to_owned()))
        })
    }
}

/// Reads a `Coblet` argument.
///
/// # Safety
///
/// `coblet` must be NULL or a live handle.
#[allow(unsafe_code, reason = "reads a handle argument")]
pub(crate) unsafe fn handle<'a>(coblet: *const Coblet) -> Result<&'a Coblet, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { coblet.as_ref() }.ok_or_else(|| Failure::argument("coblet", "is NULL"))
}

/// Hands a JSON object out with `"schema":1` added.
///
/// # Safety
///
/// `out_json` must be NULL or writable.
#[allow(unsafe_code, reason = "writes the out-parameter")]
pub(crate) unsafe fn put_json(
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

pub(crate) fn extension_json(extension: &dyn Extension) -> serde_json::Value {
    serde_json::json!({ "id": extension.id().as_str(), "name": extension.display_name() })
}

fn install_json(install: &JavaInstall) -> serde_json::Value {
    let (source, source_id, package) = match &install.source {
        InstallSource::JavaHome => ("java-home", None, None),
        InstallSource::Path => ("path", None, None),
        InstallSource::Sdkman => ("sdkman", None, None),
        InstallSource::SystemDirectory => ("system", None, None),
        InstallSource::Configured => ("configured", None, None),
        InstallSource::Directory(dir) => ("directory", Some(dir.display().to_string()), None),
        InstallSource::MojangRuntime { component } => {
            ("mojang-runtime", Some(component.clone()), None)
        }
        InstallSource::Located { locator } => ("locator", Some(locator.to_string()), None),
        InstallSource::Provider { provider, package } => (
            "provider",
            Some(provider.to_string()),
            Some(package.clone()),
        ),
        _ => ("other", None, None),
    };
    serde_json::json!({
        "executable": install.executable,
        "home": install.home,
        "version": install.version,
        "major": install.major,
        "arch": install.arch.as_str(),
        "vendor": install.vendor,
        "source": source,
        "source_id": source_id,
        "package": package,
    })
}

/// Lists the Java providers, in the order they are asked, as JSON:
/// `{"schema":1,"providers":[{"id","name"}]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_java_providers_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let providers: Vec<_> = handle
                .inner
                .java_providers()
                .iter()
                .map(|provider| extension_json(provider.as_ref()))
                .collect();
            put_json(out_json, serde_json::json!({ "providers": providers }))
        })
    }
}

/// Lists the places searched for installed Java, in order, as JSON:
/// `{"schema":1,"locators":[{"id","name"}]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// As `coblet_java_providers_json`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_java_locators_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let locators: Vec<_> = handle
                .inner
                .java_locators()
                .iter()
                .map(|locator| extension_json(locator.as_ref()))
                .collect();
            put_json(out_json, serde_json::json!({ "locators": locators }))
        })
    }
}

/// Finds and probes every installed Java the locators know of, as JSON:
/// `{"schema":1,"installs":[{"executable","home","version","major","arch",
/// "vendor","source","source_id","package"}]}` where `source` is
/// `java-home`, `path`, `sdkman`, `system`, `configured`, `directory`,
/// `mojang-runtime`, `locator`, `provider` or `other`. On success
/// `*out_json` receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// As `coblet_java_providers_json`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_java_installs_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let installs = handle.runtime.block_on(handle.inner.java_installs());
            let installs: Vec<_> = installs.iter().map(install_json).collect();
            put_json(out_json, serde_json::json!({ "installs": installs }))
        })
    }
}

/// Asks every Java provider, in order, for packages of a Java major version
/// for this platform, as JSON: `{"schema":1,"answers":[{"provider",
/// "packages","error"}]}` with packages in the format of
/// `CobletJavaProvider` and `error` null unless the provider failed. On
/// success `*out_json` receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// As `coblet_java_providers_json`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_java_packages_json(
    coblet: *const Coblet,
    major: u32,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let query = handle.inner.java_query(major);
            let answers = handle.runtime.block_on(handle.inner.java_packages(&query));
            let answers: Vec<_> = answers
                .into_iter()
                .map(|(provider, answer)| {
                    let (packages, error) = match answer {
                        Ok(packages) => (packages, None),
                        Err(error) => (Vec::new(), Some(error.to_string())),
                    };
                    serde_json::json!({
                        "provider": provider.as_str(),
                        "packages": packages,
                        "error": error,
                    })
                })
                .collect();
            put_json(out_json, serde_json::json!({ "answers": answers }))
        })
    }
}

/// Installs a package of a Java provider, as listed by
/// `coblet_java_packages_json`, and probes the result; blocks until done.
///
/// Progress arrives as `COBLET_TRANSFER_KIND_JAVA_RUNTIME` transfer events
/// of `operation_id`, from `coblet_generate_operation_id`, or of a random
/// ID when it is NULL. On success `*out_json` receives one install in the
/// format of the entries of `coblet_java_installs_json`, with `schema`, to
/// free with `coblet_string_free`. A malformed provider ID or package fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`, a failed download with
/// `COBLET_ERROR_KIND_NETWORK`.
///
/// # Safety
///
/// `coblet` must be a live handle; strings must be NULL or NUL-terminated;
/// `out_json` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_install_java(
    coblet: *const Coblet,
    provider: *const c_char,
    package_json: *const c_char,
    operation_id: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let provider = extension_id("java provider", text(provider, "provider")?)?;
            let package: JavaPackage = serde_json::from_str(text(package_json, "package_json")?)
                .map_err(|error| Failure::argument("package_json", &error.to_string()))?;
            let op = self::operation_id(operation_id)?.unwrap_or_else(OperationId::random);
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let install = handle.runtime.block_on(handle.inner.install_java(
                &provider,
                &package,
                op,
                &CancellationToken::new(),
            ))?;
            put_json(out_json, install_json(&install))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        COBLET_JAVA_REPLACE, CobletJavaCtx, CobletJavaLocator, CobletJavaProvider,
        coblet_builder_add_java_locator, coblet_builder_add_java_provider,
        coblet_builder_add_java_provider_toml, coblet_builder_remove_java_locator,
        coblet_builder_remove_java_provider, coblet_install_java, coblet_java_ctx_fail,
        coblet_java_ctx_fetch, coblet_java_ctx_set_result, coblet_java_installs_json,
        coblet_java_locators_json, coblet_java_packages_json, coblet_java_providers_json,
    };
    use crate::builder::{
        CobletBuilder, coblet_builder_build, coblet_builder_free, coblet_builder_new,
        coblet_builder_set_platform,
    };
    use crate::context::{Coblet, coblet_error_free, coblet_free, coblet_string_free};
    use crate::types::{CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString};

    const HASH: &str = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    /// Counts frees in the `AtomicUsize` that `user_data` points to, if any.
    unsafe extern "C" fn free(user_data: *mut c_void) {
        // SAFETY: the tests pass NULL or a counter that outlives the handle.
        if let Some(count) = unsafe { user_data.cast::<AtomicUsize>().as_ref() } {
            count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn set(ctx: *mut CobletJavaCtx, json: &str) -> bool {
        let json = CString::new(json).unwrap();
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_java_ctx_set_result(ctx, json.as_ptr()) == CobletStatus::Ok }
    }

    /// Offers one zip for the queried major version.
    unsafe extern "C" fn packages(
        _user_data: *mut c_void,
        ctx: *mut CobletJavaCtx,
        query: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated JSON.
        let query: serde_json::Value =
            serde_json::from_str(unsafe { CStr::from_ptr(query) }.to_str().unwrap()).unwrap();
        let package = serde_json::json!([{
            "id": format!("acme-{}-{}", query["major"], query["arch"].as_str().unwrap()),
            "version": "17.0.12",
            "major": query["major"],
            "arch": query["arch"],
            "download": {
                "type": "archive",
                "url": "https://jdk.acme.test/17.zip",
                "hash": HASH,
                "format": "zip",
                "size": 42
            }
        }]);
        set(ctx, &package.to_string())
    }

    unsafe extern "C" fn refuse(
        _user_data: *mut c_void,
        ctx: *mut CobletJavaCtx,
        _query: *const c_char,
    ) -> bool {
        let message = CString::new("mirror offline").unwrap();
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_java_ctx_fail(ctx, message.as_ptr()) };
        false
    }

    /// Calls of `locate`.
    static LOCATED: AtomicUsize = AtomicUsize::new(0);

    /// Lists one executable, after checking locators cannot fetch.
    unsafe extern "C" fn locate(
        _user_data: *mut c_void,
        ctx: *mut CobletJavaCtx,
        os: *const c_char,
    ) -> bool {
        // SAFETY: the library passes a NUL-terminated name.
        assert_eq!(unsafe { CStr::from_ptr(os) }.to_str().unwrap(), "linux");
        LOCATED.fetch_add(1, Ordering::SeqCst);
        let mut body = CobletBytes::empty();
        let mut err = std::ptr::null_mut();
        // SAFETY: the running callback's handle, valid strings and out-pointers.
        let status = unsafe {
            coblet_java_ctx_fetch(
                ctx,
                c"https://example.test/".as_ptr(),
                c"x.json".as_ptr(),
                &raw mut body,
                &raw mut err,
            )
        };
        assert_eq!(status, CobletStatus::InvalidArgument);
        // SAFETY: the error was just returned.
        unsafe { coblet_error_free(err) };
        set(ctx, r#"["/nowhere/bin/java"]"#)
    }

    fn provider(id: &CStr, user_data: *mut c_void) -> CobletJavaProvider {
        CobletJavaProvider {
            user_data,
            id: id.as_ptr(),
            name: c"Acme JDK".as_ptr(),
            packages: Some(packages),
            free: Some(free),
        }
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

    fn take_error(err: *mut CobletError) -> CobletErrorKind {
        assert!(!err.is_null());
        // SAFETY: the error was just returned and is freed once.
        unsafe {
            let kind = (*err).kind;
            coblet_error_free(err);
            kind
        }
    }

    /// A builder for Linux on `x86_64` with no built-in provider, so nothing
    /// goes to the network.
    fn builder(root: &std::path::Path) -> *mut CobletBuilder {
        let root = CString::new(root.to_str().unwrap()).unwrap();
        let mut builder = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        // SAFETY: valid strings and out-pointers.
        unsafe {
            let status = coblet_builder_new(
                root.as_ptr(),
                c"coblet-ffi-test/1.0".as_ptr(),
                &raw mut builder,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            let platform = cr#"{"os":"linux","arch":"x86_64"}"#;
            let status = coblet_builder_set_platform(builder, platform.as_ptr(), &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            for id in [c"coblet:mojang", c"coblet:adoptium", c"coblet:zulu"] {
                let status =
                    coblet_builder_remove_java_provider(builder, id.as_ptr(), &raw mut err);
                assert_eq!(status, CobletStatus::Ok);
            }
        }
        builder
    }

    fn build(builder: *mut CobletBuilder) -> Result<*mut Coblet, CobletErrorKind> {
        let mut coblet = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        // SAFETY: a live builder handed over, valid out-pointers.
        let status = unsafe { coblet_builder_build(builder, &raw mut coblet, &raw mut err) };
        if status == CobletStatus::Ok {
            Ok(coblet)
        } else {
            Err(take_error(err))
        }
    }

    #[test]
    fn c_providers_and_locators_take_part() {
        let dir = tempfile::tempdir().unwrap();
        let frees = AtomicUsize::new(0);
        let counter = std::ptr::from_ref(&frees).cast_mut().cast::<c_void>();
        let builder = builder(dir.path());
        let mut out = CobletString::empty();
        let mut err = std::ptr::null_mut();
        let broken = CobletJavaProvider {
            packages: Some(refuse),
            ..provider(c"acme:broken", counter)
        };
        let locator = CobletJavaLocator {
            user_data: counter,
            id: c"acme:elsewhere".as_ptr(),
            name: c"Elsewhere".as_ptr(),
            locate: Some(locate),
            free: Some(free),
        };
        // SAFETY: a live builder, tables with static strings, valid out-pointers.
        unsafe {
            for table in [provider(c"acme:jdk", counter), broken] {
                let status = coblet_builder_add_java_provider(builder, table, 0, &raw mut err);
                assert_eq!(status, CobletStatus::Ok);
            }
            let status = coblet_builder_add_java_locator(builder, locator, 0, &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            for id in coblet::java::BUILTIN_LOCATORS {
                let id = CString::new(id).unwrap();
                let status = coblet_builder_remove_java_locator(builder, id.as_ptr(), &raw mut err);
                assert_eq!(status, CobletStatus::Ok);
            }
            let coblet = build(builder).unwrap();

            assert_eq!(
                coblet_java_providers_json(coblet, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            let providers = take(&mut out);
            assert_eq!(providers["schema"], 1);
            assert_eq!(providers["providers"][0]["id"], "acme:jdk");
            assert_eq!(providers["providers"].as_array().unwrap().len(), 2);

            assert_eq!(
                coblet_java_locators_json(coblet, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            let locators = take(&mut out);
            assert_eq!(
                locators["locators"],
                serde_json::json!([{ "id": "acme:elsewhere", "name": "Elsewhere" }])
            );

            // Only the C locator is asked; its executable does not exist.
            assert_eq!(
                coblet_java_installs_json(coblet, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(take(&mut out)["installs"], serde_json::json!([]));
            assert_eq!(LOCATED.load(Ordering::SeqCst), 1);

            assert_eq!(
                coblet_java_packages_json(coblet, 17, &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            let answers = take(&mut out)["answers"].clone();
            assert_eq!(answers[0]["provider"], "acme:jdk");
            assert_eq!(answers[0]["packages"][0]["id"], "acme-17-x86_64");
            assert_eq!(answers[0]["packages"][0]["download"]["format"], "zip");
            assert!(answers[0]["error"].is_null());
            assert!(
                answers[1]["error"]
                    .as_str()
                    .unwrap()
                    .contains("mirror offline")
            );

            let status = coblet_install_java(
                coblet,
                c"acme:jdk".as_ptr(),
                c"{\"id\":\"x\"}".as_ptr(),
                std::ptr::null(),
                &raw mut out,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::InvalidArgument);
            assert_eq!(take_error(err), CobletErrorKind::InvalidArgument);

            coblet_free(coblet);
        }
        assert_eq!(frees.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn duplicates_flags_and_definitions_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let mut err = std::ptr::null_mut();
        let builder = builder(dir.path());
        // SAFETY: a live builder, tables with static strings, valid out-pointers.
        unsafe {
            let status =
                coblet_builder_add_java_provider_toml(builder, c"id = 3".as_ptr(), 0, &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
            let table = provider(c"acme:jdk", std::ptr::null_mut());
            let status = coblet_builder_add_java_provider(builder, table, 4, &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            take_error(err);
            let status = coblet_builder_add_java_provider(builder, table, 0, &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let status =
                coblet_builder_add_java_provider(builder, table, COBLET_JAVA_REPLACE, &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let coblet = build(builder).unwrap();
            coblet_free(coblet);

            let builder = self::builder(dir.path());
            for _ in 0..2 {
                let status = coblet_builder_add_java_provider(builder, table, 0, &raw mut err);
                assert_eq!(status, CobletStatus::Ok);
            }
            assert_eq!(
                build(builder).unwrap_err(),
                CobletErrorKind::InvalidArgument
            );

            let builder = self::builder(dir.path());
            coblet_builder_free(builder);
        }
    }
}
