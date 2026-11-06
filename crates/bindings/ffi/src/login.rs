use std::ffi::c_char;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use coblet::auth::{
    AuthFlow, CallbackSender, Interaction, LoginOptions, MICROSOFT_METHOD, Redirect,
};
use coblet::runtime::{AuthPrompt, CancellationToken};
use coblet::types::ExtensionId;

use crate::accounts::account_value;
use crate::context::Coblet;
use crate::guard::{guard, put};
use crate::types::{
    CobletError, CobletErrorKind, CobletStatus, CobletString, Failure, optional_text, text,
};

/// How the user signs in to Microsoft.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletLoginInteraction {
    /// Show a code that the user enters at a Microsoft page, on any device.
    DeviceCode = 0,
    /// Open a sign-in page; a built-in listener on `127.0.0.1` with a free
    /// port receives the redirect.
    Browser = 1,
    /// Open a sign-in page that redirects to `redirect_uri`, such as a
    /// custom URL scheme of the launcher; pass the URI the launcher
    /// receives to `coblet_login_submit_redirect`.
    BrowserRedirect = 2,
}

/// Microsoft login in progress, from `coblet_login_begin`.
pub struct CobletLogin {
    runtime: tokio::runtime::Handle,
    coblet: coblet::Coblet,
    method: ExtensionId,
    prompt: AuthPrompt,
    callback: Option<CallbackSender>,
    flow: Mutex<Option<AuthFlow>>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for CobletLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletLogin")
            .field("prompt", &self.prompt)
            .finish_non_exhaustive()
    }
}

/// Reads a login argument.
///
/// # Safety
///
/// `login` must be NULL or a live login handle.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn login_ref<'a>(login: *const CobletLogin) -> Result<&'a CobletLogin, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { login.as_ref() }.ok_or_else(|| Failure::argument("login", "is NULL"))
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Starts a Microsoft login.
///
/// `redirect_uri` is required for `COBLET_LOGIN_INTERACTION_BROWSER_REDIRECT`
/// and ignored otherwise; `login_hint` may be NULL or an e-mail address to
/// pre-fill. On success `*out_login` receives a handle to free with
/// `coblet_login_free`; read its prompt with `coblet_login_prompt_json`.
/// Fails with `COBLET_ERROR_KIND_UNSUPPORTED` when no Client ID was set
/// with `coblet_builder_set_msa_client_id`.
///
/// # Safety
///
/// `coblet` must be a live handle that outlives the login; strings must be
/// NULL or NUL-terminated; `out_login` must be writable; `out_err` must be
/// NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_login_begin(
    coblet: *const Coblet,
    interaction: CobletLoginInteraction,
    redirect_uri: *const c_char,
    login_hint: *const c_char,
    out_login: *mut *mut CobletLogin,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            if out_login.is_null() {
                return Err(Failure::argument("out_login", "is NULL"));
            }
            let interaction = match interaction {
                CobletLoginInteraction::DeviceCode => Interaction::DeviceCode,
                CobletLoginInteraction::Browser => Interaction::Browser(Redirect::loopback()),
                CobletLoginInteraction::BrowserRedirect => {
                    Interaction::Browser(Redirect::External {
                        redirect_uri: text(redirect_uri, "redirect_uri")?.to_owned(),
                    })
                }
            };
            let mut options = LoginOptions::new().with_interaction(interaction);
            if let Some(hint) = optional_text(login_hint, "login_hint")? {
                options = options.with_hint(hint);
            }
            let method = ExtensionId::new(MICROSOFT_METHOD)
                .map_err(|error| Failure::new(CobletErrorKind::Other, error.to_string()))?;
            let flow = handle
                .runtime
                .block_on(handle.inner.begin_login(&method, &options))?;
            let login = Box::new(CobletLogin {
                runtime: handle.runtime.handle().clone(),
                coblet: handle.inner.clone(),
                method,
                prompt: flow.prompt().clone(),
                callback: flow.callback().cloned(),
                flow: Mutex::new(Some(flow)),
                cancel: CancellationToken::new(),
            });
            put(out_login, Box::into_raw(login), "out_login")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Returns what the user has to do as JSON: `{"schema":1,"type":"device_code",
/// "user_code","verification_uri","expires_at","message"}`,
/// `{"schema":1,"type":"open_url","url","redirect_uri"}` or
/// `{"schema":1,"type":"nothing"}`. `expires_at` is in Unix seconds and
/// `message` may be null.
///
/// On success `*out_json` receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `login` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_login_prompt_json(
    login: *const CobletLogin,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let login = login_ref(login)?;
            let json = prompt_json(&login.prompt);
            put(out_json, CobletString::new(&json.to_string()), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Describes a login prompt as `{"schema":1,"type",...}`.
pub(crate) fn prompt_json(prompt: &AuthPrompt) -> serde_json::Value {
    match prompt {
        AuthPrompt::DeviceCode {
            user_code,
            verification_uri,
            expires_at,
            message,
        } => serde_json::json!({
            "schema": 1,
            "type": "device_code",
            "user_code": user_code,
            "verification_uri": verification_uri.as_str(),
            "expires_at": unix_seconds(*expires_at),
            "message": message,
        }),
        AuthPrompt::OpenUrl { url, redirect_uri } => serde_json::json!({
            "schema": 1,
            "type": "open_url",
            "url": url.as_str(),
            "redirect_uri": redirect_uri,
        }),
        _ => serde_json::json!({ "schema": 1, "type": "nothing" }),
    }
}

/// Hands the full URI the browser was redirected to, with its `code` and
/// `state` parameters, to a login started with
/// `COBLET_LOGIN_INTERACTION_BROWSER_REDIRECT`. May be called while
/// another thread waits in `coblet_login_finish`.
///
/// # Safety
///
/// `login` must be a live handle; `uri` must be NULL or NUL-terminated;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_login_submit_redirect(
    login: *const CobletLogin,
    uri: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let login = login_ref(login)?;
            let uri = text(uri, "uri")?;
            let callback = login.callback.as_ref().ok_or_else(|| {
                Failure::new(
                    CobletErrorKind::InvalidState,
                    "this login does not wait for a redirect",
                )
            })?;
            callback.submit(uri)?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Stops the login: a waiting or later `coblet_login_finish` fails with
/// `COBLET_ERROR_KIND_CANCELLED`. May be called from any thread; NULL is
/// ignored.
///
/// # Safety
///
/// `login` must be NULL or a live handle.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_login_cancel(login: *const CobletLogin) {
    // SAFETY: forwarded from the caller's guarantee.
    if let Some(login) = unsafe { login.as_ref() } {
        login.cancel.cancel();
    }
}

/// Waits until the user has signed in, saves the account and returns it
/// as JSON, in the account format of `coblet_accounts_json` with a
/// `schema` field. A login finishes once.
///
/// Accounts without the game or a player profile are saved with the state
/// `no-license` or `no-profile` rather than failing.
///
/// # Safety
///
/// `login` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_login_finish(
    login: *const CobletLogin,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let login = login_ref(login)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let flow = login
                .flow
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .ok_or_else(|| {
                    Failure::new(
                        CobletErrorKind::InvalidState,
                        "the login was already finished",
                    )
                })?;
            let finish = login.coblet.finish_login(&login.method, flow);
            let account = login
                .runtime
                .block_on(login.cancel.run_until_cancelled(finish))
                .ok_or_else(|| {
                    Failure::new(CobletErrorKind::Cancelled, "the login was cancelled")
                })??;
            let mut json = account_value(&account);
            json["schema"] = 1.into();
            put(out_json, CobletString::new(&json.to_string()), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Frees a login handle; NULL is ignored. An unfinished login stops.
///
/// # Safety
///
/// `login` must be NULL or a handle from `coblet_login_begin` that no other
/// thread uses any more.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_login_free(login: *mut CobletLogin) {
    if !login.is_null() {
        // SAFETY: the handle came from `Box::into_raw` in `coblet_login_begin`.
        drop(unsafe { Box::from_raw(login) });
    }
}

#[cfg(test)]
#[path = "../tests/login.rs"]
mod tests;
