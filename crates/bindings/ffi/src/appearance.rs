use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use coblet::auth::{PlayerProfile, SkinModel};
use coblet::player::{AppearanceCaps, AppearanceCtx, PlayerError, SkinSource, check_skin_png};
use coblet::runtime::{BoxFuture, Extension, Transport};
use coblet::types::ExtensionId;

use crate::accounts::{account_id, account_value};
use crate::auth_methods::{PlayerJson, extension_id};
use crate::builder::{CobletBuilder, builder_mut};
use crate::context::Coblet;
use crate::customize::Owned;
use crate::guard::{guard, put};
use crate::java::{extension_json, handle, put_json};
use crate::types::{
    CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString, Failure, text,
};

/// Flag of `coblet_builder_add_appearance_service`: put the service in
/// place of the one with the same ID, such as `coblet:minecraft-services`,
/// instead of failing when the `Coblet` is built.
pub const COBLET_APPEARANCE_REPLACE: u32 = 1;

/// Capability of a `CobletAppearanceService`: uploads skins from PNG bytes.
pub const COBLET_APPEARANCE_CAN_UPLOAD_PNG: u32 = 1;
/// Capability: uploads skins from a public URL.
pub const COBLET_APPEARANCE_CAN_UPLOAD_URL: u32 = 1 << 1;
/// Capability: switches between the Classic and Slim models.
pub const COBLET_APPEARANCE_CAN_SET_MODEL: u32 = 1 << 2;
/// Capability: goes back to the default skin.
pub const COBLET_APPEARANCE_CAN_RESET_SKIN: u32 = 1 << 3;
/// Capability: shows one of the capes the account owns.
pub const COBLET_APPEARANCE_CAN_CHOOSE_CAPE: u32 = 1 << 4;
/// Capability: hides the cape.
pub const COBLET_APPEARANCE_CAN_HIDE_CAPE: u32 = 1 << 5;

type Callback = unsafe extern "C" fn(*mut c_void, *mut CobletAppearanceCtx, *const c_char) -> bool;

/// Callback table of a service that shows and changes skins and capes,
/// written in C, such as a skin site with its own API.
///
/// `id`, `name` and `capabilities` (`COBLET_APPEARANCE_CAN_*` flags) are
/// copied when the table is added; the library asks only for what the
/// flags allow and checks skin images first. `call` runs on a library
/// worker thread where blocking is fine, gets a `CobletAppearanceCtx`
/// handle valid only during the call and a request as JSON:
/// `{"schema":1,"op","account"}` with the account in the format of
/// `coblet_accounts_json` and `op` one of `appearance`, `upload_skin_png`
/// (with `model`; read the image with `coblet_appearance_ctx_png`),
/// `upload_skin_url` (with `url` and `model`), `set_model` (with `model`),
/// `reset_skin` and `set_cape` (with `cape`, an ID or null to hide it).
/// `model` is `classic` or `slim`. It answers with the player profile as
/// it is afterwards, in the format of `coblet_accounts_json`, through
/// `coblet_appearance_ctx_set_result` and returns true, or fails with
/// `coblet_appearance_ctx_fail` and returns false;
/// `COBLET_ERROR_KIND_AUTH` makes the library renew the token and ask
/// once more. Ownership of `user_data` is as for `CobletAuthMethod`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletAppearanceService {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:skins`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// `COBLET_APPEARANCE_CAN_*` flags.
    pub capabilities: u32,
    /// Reads or changes the appearance. Returns false on failure.
    pub call: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletAppearanceCtx,
            request_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// One appearance service change, applied in order.
pub(crate) enum AppearanceSetting {
    Add(Arc<dyn coblet::player::AppearanceService>, bool),
    Remove(ExtensionId),
    Toml(String),
    Json(String),
    Bind(ExtensionId, ExtensionId),
}

impl AppearanceSetting {
    /// Applies the change to the engine builder.
    pub(crate) fn apply(
        self,
        builder: coblet::CobletBuilder<coblet::WithRoot>,
    ) -> coblet::CobletBuilder<coblet::WithRoot> {
        match self {
            Self::Add(service, false) => builder.appearance_service(service),
            Self::Add(service, true) => builder.replace_appearance_service(service),
            Self::Remove(id) => builder.without_appearance_service(id),
            Self::Toml(definition) => builder.appearance_service_toml(definition),
            Self::Json(definition) => builder.appearance_service_json(definition),
            Self::Bind(method, service) => builder.appearance_for(method, service),
        }
    }
}

/// Turns `COBLET_APPEARANCE_CAN_*` flags into capabilities.
pub(crate) fn caps(flags: u32) -> AppearanceCaps {
    let on = |flag: u32| flags & flag != 0;
    AppearanceCaps::none()
        .with_upload_png(on(COBLET_APPEARANCE_CAN_UPLOAD_PNG))
        .with_upload_url(on(COBLET_APPEARANCE_CAN_UPLOAD_URL))
        .with_set_model(on(COBLET_APPEARANCE_CAN_SET_MODEL))
        .with_reset_skin(on(COBLET_APPEARANCE_CAN_RESET_SKIN))
        .with_choose_cape(on(COBLET_APPEARANCE_CAN_CHOOSE_CAPE))
        .with_hide_cape(on(COBLET_APPEARANCE_CAN_HIDE_CAPE))
}

/// Describes capabilities as `{"upload_png",...}`.
pub(crate) fn caps_value(caps: AppearanceCaps) -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "upload_png": caps.upload_png,
        "upload_url": caps.upload_url,
        "set_model": caps.set_model,
        "reset_skin": caps.reset_skin,
        "choose_cape": caps.choose_cape,
        "hide_cape": caps.hide_cape,
    })
}

/// One running appearance callback: its account and its answer.
pub struct CobletAppearanceCtx {
    runtime: tokio::runtime::Handle,
    transport: Arc<dyn Transport>,
    token: Option<String>,
    png: Option<Vec<u8>>,
    result: Option<String>,
    failure: Option<(CobletErrorKind, String)>,
}

impl std::fmt::Debug for CobletAppearanceCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletAppearanceCtx")
            .finish_non_exhaustive()
    }
}

#[allow(unsafe_code, reason = "calls a C appearance callback")]
fn invoke(
    owned: &Owned,
    callback: Callback,
    ctx: &mut CobletAppearanceCtx,
    request: &CString,
) -> bool {
    // SAFETY: the table owner promised a callback that accepts its user
    // data, the context handle and a NUL-terminated string, all valid for
    // the duration of the call.
    unsafe { callback(owned.user_data, ctx, request.as_ptr()) }
}

/// C appearance service used as a coblet appearance service.
pub(crate) struct ForeignAppearance {
    id: ExtensionId,
    name: String,
    caps: AppearanceCaps,
    call: Callback,
    owned: Arc<Owned>,
}

impl ForeignAppearance {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    pub(crate) unsafe fn new(table: &CobletAppearanceService) -> Result<Self, Failure> {
        let owned = Arc::new(Owned {
            user_data: table.user_data,
            free: table.free,
        });
        let call = table
            .call
            .ok_or_else(|| Failure::argument("appearance service", "has no callback"))?;
        // SAFETY: forwarded from the caller's guarantee.
        let (id, name) = unsafe { (text(table.id, "id")?, text(table.name, "name")?) };
        Ok(Self {
            id: extension_id("appearance service", id)?,
            name: name.to_owned(),
            caps: caps(table.capabilities),
            call,
            owned,
        })
    }

    fn run<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        operation: &'static str,
        mut request: serde_json::Value,
        png: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        Box::pin(async move {
            request["schema"] = 1.into();
            request["account"] = account_value(cx.account());
            let request = CString::new(request.to_string())
                .map_err(|_| PlayerError::Failed("the request contains NUL".to_owned()))?;
            let mut ctx = CobletAppearanceCtx {
                runtime: tokio::runtime::Handle::current(),
                transport: Arc::clone(cx.transport()),
                token: cx
                    .access_token()
                    .ok()
                    .map(|token| token.expose().to_owned()),
                png,
                result: None,
                failure: None,
            };
            let (owned, call) = (Arc::clone(&self.owned), self.call);
            let (succeeded, ctx) = tokio::task::spawn_blocking(move || {
                let succeeded = invoke(&owned, call, &mut ctx, &request);
                (succeeded, ctx)
            })
            .await
            .map_err(|_| PlayerError::Failed("the appearance service stopped".to_owned()))?;
            let json = match (succeeded, ctx.result) {
                (true, Some(json)) => json,
                (true, None) => {
                    return Err(PlayerError::Failed("the callback set no result".to_owned()));
                }
                (false, _) => {
                    let (kind, message) = ctx
                        .failure
                        .unwrap_or((CobletErrorKind::Other, "the callback failed".to_owned()));
                    return Err(self.failed(kind, message, operation));
                }
            };
            let invalid = |message: String| PlayerError::InvalidResponse {
                endpoint: "appearance service",
                message,
            };
            serde_json::from_str::<PlayerJson>(&json)
                .map_err(|error| invalid(error.to_string()))?
                .into_profile()
                .map_err(invalid)
        })
    }

    fn failed(
        &self,
        kind: CobletErrorKind,
        message: String,
        operation: &'static str,
    ) -> PlayerError {
        match kind {
            CobletErrorKind::Auth => PlayerError::Unauthorized,
            CobletErrorKind::Unsupported => PlayerError::Unsupported {
                service: self.id.clone(),
                operation,
            },
            CobletErrorKind::InvalidArgument => PlayerError::InvalidSkin(message),
            CobletErrorKind::InvalidState => PlayerError::NoSkin,
            _ => PlayerError::Failed(message),
        }
    }
}

impl Extension for ForeignAppearance {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl coblet::player::AppearanceService for ForeignAppearance {
    fn capabilities(&self) -> AppearanceCaps {
        self.caps
    }

    fn appearance<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let request = serde_json::json!({ "op": "appearance" });
        self.run(cx, "read the appearance", request, None)
    }

    fn upload_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        source: SkinSource,
        model: SkinModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        match source {
            SkinSource::Png(png) => {
                if let Err(error) = check_skin_png(&png) {
                    return Box::pin(async move { Err(error) });
                }
                let request =
                    serde_json::json!({ "op": "upload_skin_png", "model": model.as_str() });
                self.run(cx, "upload skins from a file", request, Some(png))
            }
            SkinSource::Url(url) => {
                let request = serde_json::json!({
                    "op": "upload_skin_url",
                    "url": url.as_str(),
                    "model": model.as_str(),
                });
                self.run(cx, "upload skins from a URL", request, None)
            }
            _ => {
                Box::pin(async move { Err(PlayerError::Failed("unknown skin source".to_owned())) })
            }
        }
    }

    fn set_model<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        model: SkinModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let request = serde_json::json!({ "op": "set_model", "model": model.as_str() });
        self.run(cx, "change the skin model", request, None)
    }

    fn reset_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let request = serde_json::json!({ "op": "reset_skin" });
        self.run(cx, "reset skins", request, None)
    }

    fn set_cape<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        cape: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let operation = if cape.is_some() {
            "choose a cape"
        } else {
            "hide the cape"
        };
        let request = serde_json::json!({ "op": "set_cape", "cape": cape });
        self.run(cx, operation, request, None)
    }
}

/// Reads a context handle argument.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(
    ctx: *mut CobletAppearanceCtx,
) -> Result<&'a mut CobletAppearanceCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Points `*out_png` and `*out_len` at the image of an `upload_skin_png`
/// request; the bytes stay valid during the call. Fails with
/// `COBLET_ERROR_KIND_INVALID_STATE` for other requests.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; the
/// out-parameters must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_appearance_ctx_png(
    ctx: *mut CobletAppearanceCtx,
    out_png: *mut *const u8,
    out_len: *mut usize,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let png = ctx.png.as_ref().ok_or_else(|| {
                Failure::new(
                    CobletErrorKind::InvalidState,
                    "this request carries no image",
                )
            })?;
            if out_png.is_null() || out_len.is_null() {
                return Err(Failure::argument("out_png or out_len", "is NULL"));
            }
            put(out_png, png.as_ptr(), "out_png")?;
            put(out_len, png.len(), "out_len")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Returns the access token of the account's current session. On success
/// `*out_token` receives a string to free with `coblet_string_free`.
/// Fails with `COBLET_ERROR_KIND_UNSUPPORTED` for offline sessions.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `out_token`
/// must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_appearance_ctx_access_token(
    ctx: *mut CobletAppearanceCtx,
    out_token: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let token = ctx
                .token
                .as_deref()
                .ok_or_else(|| Failure::from(coblet::Error::Player(PlayerError::Offline)))?;
            put(out_token, CobletString::new(token), "out_token")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Sends an HTTP request like `coblet_auth_ctx_send`.
///
/// # Safety
///
/// As for `coblet_auth_ctx_send`, with an appearance context.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_appearance_ctx_send(
    ctx: *mut CobletAppearanceCtx,
    request_json: *const c_char,
    body: *const u8,
    body_len: usize,
    out_response_json: *mut CobletString,
    out_body: *mut CobletBytes,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let body = crate::http::body_arg(body, body_len);
            let request = text(request_json, "request_json")?;
            crate::http::answer(
                crate::http::send(&ctx.runtime, &ctx.transport, request, body)?,
                out_response_json,
                out_body,
            )
        })
    }
}

/// Hands the player profile the running appearance callback answers with
/// to the library, which copies it.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `json` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_appearance_ctx_set_result(
    ctx: *mut CobletAppearanceCtx,
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

/// Gives the kind and reason of the failure the running appearance
/// callback is about to return false for.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_appearance_ctx_fail(
    ctx: *mut CobletAppearanceCtx,
    kind: CobletErrorKind,
    message: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            ctx_mut(ctx)?.failure = Some((kind, text(message, "message")?.to_owned()));
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds an appearance change to a builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder handle; `out_err` must be NULL
/// or writable.
#[allow(unsafe_code, reason = "reads the builder handle")]
unsafe fn push(
    builder: *mut CobletBuilder,
    out_err: *mut *mut CobletError,
    change: impl FnOnce() -> Result<AppearanceSetting, Failure>,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            builder.appearance.push(change()?);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a service written in C that shows and changes skins and capes.
/// Auth methods name it, or `coblet_builder_set_appearance_for` binds one
/// to it.
///
/// `flags` is 0 or `COBLET_APPEARANCE_REPLACE`. Ownership and failures are
/// as for `coblet_builder_add_auth_method`.
///
/// # Safety
///
/// As for `coblet_builder_add_auth_method`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_appearance_service(
    builder: *mut CobletBuilder,
    service: CobletAppearanceService,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignAppearance::new(&service) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, move || {
            if flags & !COBLET_APPEARANCE_REPLACE != 0 {
                return Err(Failure::argument(
                    "flags",
                    &format!("unknown flags {flags:#x}"),
                ));
            }
            let replace = flags & COBLET_APPEARANCE_REPLACE != 0;
            Ok(AppearanceSetting::Add(Arc::new(adapter?), replace))
        })
    }
}

/// Removes an appearance service, such as `coblet:minecraft-services`; an
/// unknown ID is ignored.
///
/// # Safety
///
/// `builder` must be a live builder handle; `id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_remove_appearance_service(
    builder: *mut CobletBuilder,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let id = extension_id("appearance service", text(id, "id")?)?;
            Ok(AppearanceSetting::Remove(id))
        })
    }
}

/// Adds an appearance service from a TOML definition, such as a skin site
/// that follows the authlib-injector specification: `id`, `name`
/// (optional), `kind = "yggdrasil"` and `api_root`. A malformed definition
/// fails `coblet_builder_build` with `COBLET_ERROR_KIND_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `definition` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_appearance_service_toml(
    builder: *mut CobletBuilder,
    definition: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            Ok(AppearanceSetting::Toml(
                text(definition, "definition")?.to_owned(),
            ))
        })
    }
}

/// Adds an appearance service from a JSON definition with the fields of
/// the TOML form.
///
/// # Safety
///
/// As for `coblet_builder_add_appearance_service_toml`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_appearance_service_json(
    builder: *mut CobletBuilder,
    definition: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            Ok(AppearanceSetting::Json(
                text(definition, "definition")?.to_owned(),
            ))
        })
    }
}

/// Shows and changes the skins and capes of `method`'s accounts with
/// `service`, instead of the service the method names. A service ID that
/// is not registered fails `coblet_builder_build` with
/// `COBLET_ERROR_KIND_UNSUPPORTED`.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_set_appearance_for(
    builder: *mut CobletBuilder,
    method: *const c_char,
    service: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        push(builder, out_err, || {
            let method = extension_id("auth method", text(method, "method")?)?;
            let service = extension_id("appearance service", text(service, "service")?)?;
            Ok(AppearanceSetting::Bind(method, service))
        })
    }
}

/// Lists the appearance services as JSON:
/// `{"schema":1,"services":[{"id","name"}]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_appearance_services_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let services: Vec<_> = handle
                .inner
                .appearance_services()
                .iter()
                .map(|service| extension_json(service.as_ref()))
                .collect();
            put_json(out_json, serde_json::json!({ "services": services }))
        })
    }
}

/// Returns what the account's appearance service can change as JSON:
/// `{"schema":1,"upload_png","upload_url","set_model","reset_skin",
/// "choose_cape","hide_cape"}`, all false for accounts without a service.
/// On success `*out_json` receives a string to free with
/// `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `account_id` must be NULL or
/// NUL-terminated; `out_json` must be writable; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_appearance_capabilities_json(
    coblet: *const Coblet,
    account_id: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let id = self::account_id(account_id, "account_id")?;
            let caps = handle
                .runtime
                .block_on(handle.inner.appearance_capabilities(&id))?;
            put_json(out_json, caps_value(caps))
        })
    }
}
