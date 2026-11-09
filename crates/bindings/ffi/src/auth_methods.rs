use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coblet::auth::{
    Account, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError, AuthFlow,
    CallbackReceiver, Cape, Credentials, Interaction, LaunchSession, LoginOptions, PlayerProfile,
    Redirect, RefreshMode, Refreshed, Skin, SkinModel, callback_channel,
};
use coblet::launch_command::{Credential, LaunchAuth, OnlineSession};
use coblet::runtime::{
    AuthPrompt, BoxFuture, CancellationToken, Extension, HttpUrl, Secret, Transport,
};
use coblet::types::{ExtensionId, PlayerName};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::accounts::account_value;
use crate::builder::{CobletBuilder, builder_mut};
use crate::context::Coblet;
use crate::customize::Owned;
use crate::guard::{guard, put};
use crate::java::{extension_json, handle, put_json};
use crate::types::{
    CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString, Failure, text,
};

/// Flag of `coblet_builder_add_auth_method`: put the method in place of
/// the one with the same ID, such as `coblet:offline`, instead of failing
/// when the `Coblet` is built.
pub const COBLET_AUTH_REPLACE: u32 = 1;

type Callback = unsafe extern "C" fn(*mut c_void, *mut CobletAuthCtx, *const c_char) -> bool;

/// Callback table of a way of signing in written in C, such as a login
/// server that follows the authlib-injector specification.
///
/// `id`, `name` and `appearance_service` (the ID of the service for the
/// accounts' skins and capes, or NULL) are copied when the table is added.
/// Both callbacks run on a library worker thread where blocking is fine,
/// get a `CobletAuthCtx` handle valid only during the call and a request
/// as JSON, answer with `coblet_auth_ctx_set_result` and return true, or
/// give a reason with `coblet_auth_ctx_fail` and return false.
///
/// `login` gets `{"schema":1,"login_hint","interaction","params",
/// "secrets"}`: `interaction` is `{"type":"device_code"}`,
/// `{"type":"browser_loopback","host","port","path"}` or
/// `{"type":"browser_redirect","redirect_uri"}`; `params` and `secrets` are
/// string maps the launcher passed to `coblet_login_begin_with`. It calls
/// `coblet_auth_ctx_show_prompt` once when the user has something to do;
/// the launcher gets the prompt while `login` keeps running. Its result is
/// the account change: `{"player","state","session_expires_at","data",
/// "secrets"}` with `player` null or in the format of
/// `coblet_accounts_json`, `state` such as `ready`, `session_expires_at`
/// in Unix seconds or null, and string maps `data` (kept with the account)
/// and `secrets` (saved in the secret store).
///
/// `refresh` gets `{"schema":1,"account","mode"}` with the account in the
/// format of `coblet_accounts_json` and `mode` `if-needed`, `force` or
/// `recheck`. It reads and saves secrets with `coblet_auth_ctx_secret` and
/// `coblet_auth_ctx_put_secret`, and answers `{"change","session"}` with
/// an account change and a session, or null when the account cannot
/// play: `{"player_name","uuid","access_token","xuid","client_id",
/// "user_type","jvm_args","expires_at"}`, where a null `access_token`
/// launches offline and `jvm_args` such as a `-javaagent` are added to the
/// launch. Failing with `COBLET_ERROR_KIND_AUTH` means the user has to
/// sign in again.
///
/// The library owns `user_data` from the call that receives the table,
/// even when it fails, and calls `free` once when the `Coblet` is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletAuthMethod {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Namespaced ID, such as `acme:login`.
    pub id: *const c_char,
    /// Name shown to users.
    pub name: *const c_char,
    /// ID of the appearance service of the accounts, or NULL.
    pub appearance_service: *const c_char,
    /// Signs in. Returns false on failure.
    pub login: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletAuthCtx,
            request_json: *const c_char,
        ) -> bool,
    >,
    /// Returns a session to launch with. Returns false on failure.
    pub refresh: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            ctx: *mut CobletAuthCtx,
            request_json: *const c_char,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// Secret call of a running refresh; the refresh future runs it, since it
/// holds the account's secrets.
enum SecretCall {
    Get(String, mpsc::SyncSender<Result<Option<Secret>, AuthError>>),
    Put(String, Secret, mpsc::SyncSender<Result<(), AuthError>>),
}

impl SecretCall {
    async fn run(self, secrets: &AccountSecrets) {
        match self {
            Self::Get(name, answer) => {
                let _ = answer.send(secrets.get(&name).await);
            }
            Self::Put(name, secret, answer) => {
                let _ = answer.send(secrets.put(&name, secret).await);
            }
        }
    }
}

/// One running `login` or `refresh` callback: its requests and its answer.
pub struct CobletAuthCtx {
    runtime: tokio::runtime::Handle,
    transport: Arc<dyn Transport>,
    /// Cancelled when the launcher cancels or drops the login.
    cancel: CancellationToken,
    /// Login only: where the prompt is published.
    auth: Option<AuthCtx>,
    prompt: Option<oneshot::Sender<Option<AuthPrompt>>>,
    redirect: Option<CallbackReceiver>,
    /// Refresh only.
    secrets: Option<tokio::sync::mpsc::UnboundedSender<SecretCall>>,
    result: Option<String>,
    failure: Option<(CobletErrorKind, String)>,
}

impl std::fmt::Debug for CobletAuthCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletAuthCtx").finish_non_exhaustive()
    }
}

impl CobletAuthCtx {
    fn new(runtime: tokio::runtime::Handle, ctx: &AuthCtx, cancel: CancellationToken) -> Self {
        Self {
            runtime,
            transport: Arc::clone(ctx.transport()),
            cancel,
            auth: None,
            prompt: None,
            redirect: None,
            secrets: None,
            result: None,
            failure: None,
        }
    }

    /// Returns the answer, or the auth error the callback's failure means.
    fn answer(&self, succeeded: bool) -> Result<String, AuthError> {
        match (succeeded, &self.result) {
            (true, Some(result)) => Ok(result.clone()),
            (true, None) => Err(AuthError::Failed("the callback set no result".to_owned())),
            (false, _) => Err(match &self.failure {
                Some((kind, message)) => failed(*kind, message),
                None => AuthError::Failed("the callback failed".to_owned()),
            }),
        }
    }

    fn secret_call<T>(
        &self,
        call: impl FnOnce(mpsc::SyncSender<Result<T, AuthError>>) -> SecretCall,
    ) -> Result<T, Failure> {
        let ended = || Failure::new(CobletErrorKind::InvalidState, "the refresh has ended");
        let calls = self.secrets.as_ref().ok_or_else(|| {
            Failure::new(CobletErrorKind::InvalidState, "only refreshes have secrets")
        })?;
        let (sender, answer) = mpsc::sync_channel(1);
        calls.send(call(sender)).map_err(|_| ended())?;
        Ok(answer.recv().map_err(|_| ended())??)
    }
}

/// Turns the kind a C callback failed with into an auth error;
/// `COBLET_ERROR_KIND_AUTH` means the user has to sign in again.
fn failed(kind: CobletErrorKind, message: &str) -> AuthError {
    match kind {
        CobletErrorKind::Cancelled => AuthError::Cancelled,
        CobletErrorKind::Auth => AuthError::InvalidGrant,
        CobletErrorKind::NoLicense => AuthError::NoLicense,
        CobletErrorKind::NoProfile => AuthError::NoProfile,
        _ => AuthError::Failed(message.to_owned()),
    }
}

fn invalid(what: &str, error: &dyn std::fmt::Display) -> AuthError {
    AuthError::Failed(format!("invalid {what}: {error}"))
}

fn from_unix(seconds: Option<u64>) -> Option<SystemTime> {
    seconds.map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkinJson {
    id: String,
    url: String,
    model: String,
    active: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapeJson {
    id: String,
    #[serde(default)]
    alias: Option<String>,
    url: String,
    active: bool,
}

/// Player profile in the format of `coblet_accounts_json`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlayerJson {
    name: String,
    uuid: String,
    #[serde(default)]
    skins: Vec<SkinJson>,
    #[serde(default)]
    capes: Vec<CapeJson>,
}

impl PlayerJson {
    pub(crate) fn into_profile(self) -> Result<PlayerProfile, String> {
        let url = |url: String| HttpUrl::new(url).map_err(|error| error.to_string());
        let name = PlayerName::new(self.name).map_err(|error| error.to_string())?;
        let mut profile =
            PlayerProfile::new(name, &self.uuid).map_err(|error| error.to_string())?;
        for skin in self.skins {
            let model = SkinModel::parse(&skin.model)
                .ok_or_else(|| format!("unknown skin model `{}`", skin.model))?;
            profile
                .skins
                .push(Skin::new(skin.id, url(skin.url)?, model, skin.active));
        }
        for cape in self.capes {
            profile
                .capes
                .push(Cape::new(cape.id, cape.alias, url(cape.url)?, cape.active));
        }
        Ok(profile)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeJson {
    #[serde(default)]
    player: Option<PlayerJson>,
    state: String,
    #[serde(default)]
    session_expires_at: Option<u64>,
    #[serde(default)]
    data: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

impl ChangeJson {
    fn into_update(self) -> Result<AccountUpdate, AuthError> {
        let state = AccountState::parse(&self.state)
            .ok_or_else(|| invalid("account change", &format!("unknown state `{}`", self.state)))?;
        let mut update = AccountUpdate::new(state);
        if let Some(player) = self.player {
            let profile = player
                .into_profile()
                .map_err(|error| invalid("player", &error))?;
            update = update.with_profile(profile);
        }
        update.session_expires_at = from_unix(self.session_expires_at);
        update.data = self.data;
        let mut secrets = Credentials::new();
        for (name, value) in self.secrets {
            secrets.insert(name, Secret::new(value));
        }
        update.secrets = secrets;
        Ok(update)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionJson {
    player_name: String,
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    xuid: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    user_type: Option<String>,
    #[serde(default)]
    jvm_args: Vec<String>,
    #[serde(default)]
    expires_at: Option<u64>,
}

impl SessionJson {
    fn into_session(self) -> Result<LaunchSession, AuthError> {
        let name = PlayerName::new(self.player_name)?;
        let auth = match self.access_token {
            None => LaunchAuth::offline(name),
            Some(token) => {
                let mut online = OnlineSession::new(name, self.uuid, Credential::new(token));
                online.xuid = self.xuid.map(Credential::new);
                online.client_id = self.client_id.map(Credential::new);
                online.user_type = self.user_type;
                LaunchAuth::online(online)
            }
        };
        let mut session = LaunchSession::new(auth).with_jvm_args(self.jvm_args);
        session.expires_at = from_unix(self.expires_at);
        Ok(session)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshJson {
    change: ChangeJson,
    #[serde(default)]
    session: Option<SessionJson>,
}

/// Prompt handed over as JSON, in the format of `coblet_login_prompt_json`.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum PromptJson {
    DeviceCode {
        user_code: String,
        verification_uri: String,
        expires_at: u64,
        #[serde(default)]
        message: Option<String>,
    },
    OpenUrl {
        url: String,
        redirect_uri: String,
    },
    Nothing,
}

impl PromptJson {
    fn into_prompt(self) -> Result<AuthPrompt, Failure> {
        let url = |url: String| {
            HttpUrl::new(url).map_err(|error| Failure::argument("prompt_json", &error.to_string()))
        };
        Ok(match self {
            Self::DeviceCode {
                user_code,
                verification_uri,
                expires_at,
                message,
            } => AuthPrompt::DeviceCode {
                user_code,
                verification_uri: url(verification_uri)?,
                expires_at: UNIX_EPOCH + Duration::from_secs(expires_at),
                message,
            },
            Self::OpenUrl {
                url: page,
                redirect_uri,
            } => AuthPrompt::OpenUrl {
                url: url(page)?,
                redirect_uri,
            },
            Self::Nothing => AuthPrompt::Nothing,
        })
    }
}

/// Describes how the user signs in, for the login request.
pub(crate) fn interaction_value(interaction: &Interaction) -> serde_json::Value {
    match interaction {
        Interaction::Browser(Redirect::External { redirect_uri }) => serde_json::json!({
            "type": "browser_redirect",
            "redirect_uri": redirect_uri,
        }),
        Interaction::Browser(Redirect::Loopback { host, port, path }) => serde_json::json!({
            "type": "browser_loopback",
            "host": host,
            "port": port,
            "path": path,
        }),
        _ => serde_json::json!({ "type": "device_code" }),
    }
}

fn mode_name(mode: RefreshMode) -> &'static str {
    match mode {
        RefreshMode::Force => "force",
        RefreshMode::Recheck => "recheck",
        RefreshMode::IfNeeded => "if-needed",
    }
}

#[allow(unsafe_code, reason = "calls a C auth callback")]
fn invoke(owned: &Owned, callback: Callback, ctx: &mut CobletAuthCtx, request: &CString) -> bool {
    // SAFETY: the table owner promised a callback that accepts its user
    // data, the context handle and a NUL-terminated string, all valid for
    // the duration of the call.
    unsafe { callback(owned.user_data, ctx, request.as_ptr()) }
}

fn request_text(value: &serde_json::Value) -> Result<CString, AuthError> {
    CString::new(value.to_string())
        .map_err(|_| AuthError::InvalidOptions("the request contains NUL".to_owned()))
}

/// C auth method used as a coblet auth method.
pub(crate) struct ForeignAuthMethod {
    id: ExtensionId,
    name: String,
    appearance: Option<ExtensionId>,
    login: Callback,
    refresh: Callback,
    owned: Arc<Owned>,
}

impl ForeignAuthMethod {
    /// Takes over a callback table.
    ///
    /// # Safety
    ///
    /// The table's strings must be NULL or NUL-terminated.
    #[allow(unsafe_code, reason = "reads the table's strings")]
    pub(crate) unsafe fn new(table: &CobletAuthMethod) -> Result<Self, Failure> {
        let owned = Arc::new(Owned {
            user_data: table.user_data,
            free: table.free,
        });
        let missing = || Failure::argument("auth method", "has no login or refresh callback");
        let (login, refresh) = (
            table.login.ok_or_else(missing)?,
            table.refresh.ok_or_else(missing)?,
        );
        // SAFETY: forwarded from the caller's guarantee.
        let (id, name) = unsafe { (text(table.id, "id")?, text(table.name, "name")?) };
        let appearance = if table.appearance_service.is_null() {
            None
        } else {
            // SAFETY: forwarded from the caller's guarantee.
            let id = unsafe { text(table.appearance_service, "appearance_service") }?;
            Some(extension_id("appearance service", id)?)
        };
        Ok(Self {
            id: extension_id("auth method", id)?,
            name: name.to_owned(),
            appearance,
            login,
            refresh,
            owned,
        })
    }
}

pub(crate) fn extension_id(what: &str, id: &str) -> Result<ExtensionId, Failure> {
    ExtensionId::new(id)
        .map_err(|error| Failure::argument(&format!("{what} id"), &error.to_string()))
}

impl Extension for ForeignAuthMethod {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl coblet::auth::AuthMethod for ForeignAuthMethod {
    fn begin<'a>(
        &'a self,
        ctx: &'a AuthCtx,
        options: &'a LoginOptions,
    ) -> BoxFuture<'a, Result<AuthFlow, AuthError>> {
        Box::pin(async move {
            let abandoned = ctx.cancel().child_token();
            let (prompt_sender, prompt) = oneshot::channel();
            let (done_sender, done) = oneshot::channel();
            let wants_redirect = matches!(
                options.interaction,
                Interaction::Browser(Redirect::External { .. })
            );
            let mut call =
                CobletAuthCtx::new(tokio::runtime::Handle::current(), ctx, abandoned.clone());
            call.auth = Some(ctx.clone());
            call.prompt = Some(prompt_sender);
            let callback = if wants_redirect {
                let (sender, receiver) = callback_channel();
                call.redirect = Some(receiver);
                Some(sender)
            } else {
                None
            };
            let secrets: BTreeMap<&str, &str> = options
                .secrets
                .iter()
                .map(|(name, secret)| (name, secret.expose()))
                .collect();
            let request = request_text(&serde_json::json!({
                "schema": 1,
                "login_hint": options.login_hint,
                "interaction": interaction_value(&options.interaction),
                "params": options.params,
                "secrets": secrets,
            }))?;
            let (owned, login) = (Arc::clone(&self.owned), self.login);
            tokio::task::spawn_blocking(move || {
                let succeeded = invoke(&owned, login, &mut call, &request);
                if let Some(sender) = call.prompt.take() {
                    let _ = sender.send(None);
                }
                let _ = done_sender.send(call.answer(succeeded));
            });
            // Dropping the flow, also unpolled, tells the login to stop.
            let guard = abandoned.drop_guard();
            let shown = prompt.await.ok().flatten().unwrap_or(AuthPrompt::Nothing);
            let outcome = async move {
                let _guard = guard;
                let json = done
                    .await
                    .map_err(|_| AuthError::Failed("the login stopped".to_owned()))??;
                serde_json::from_str::<ChangeJson>(&json)
                    .map_err(|error| invalid("account change", &error))?
                    .into_update()
            };
            let flow = AuthFlow::new(shown, outcome);
            Ok(match callback {
                Some(sender) => flow.with_callback(sender),
                None => flow,
            })
        })
    }

    fn refresh<'a>(
        &'a self,
        ctx: &'a AuthCtx,
        account: &'a Account,
        secrets: &'a AccountSecrets,
        mode: RefreshMode,
    ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
        Box::pin(async move {
            let (calls, mut requests) = tokio::sync::mpsc::unbounded_channel();
            let mut call =
                CobletAuthCtx::new(tokio::runtime::Handle::current(), ctx, ctx.cancel().clone());
            call.secrets = Some(calls);
            let request = request_text(&serde_json::json!({
                "schema": 1,
                "account": account_value(account),
                "mode": mode_name(mode),
            }))?;
            let (owned, refresh) = (Arc::clone(&self.owned), self.refresh);
            let mut task = tokio::task::spawn_blocking(move || {
                let succeeded = invoke(&owned, refresh, &mut call, &request);
                call.answer(succeeded)
            });
            // Secret calls are served here, where the secrets live, until
            // the callback returns.
            let json = loop {
                tokio::select! {
                    joined = &mut task => break joined,
                    Some(call) = requests.recv() => call.run(secrets).await,
                }
            }
            .map_err(|_| AuthError::Failed("the refresh stopped".to_owned()))??;
            let answer: RefreshJson =
                serde_json::from_str(&json).map_err(|error| invalid("refresh result", &error))?;
            let update = answer.change.into_update()?;
            let session = answer.session.map(SessionJson::into_session).transpose()?;
            Ok(Refreshed::new(update, session))
        })
    }

    fn appearance_service(&self) -> Option<ExtensionId> {
        self.appearance.clone()
    }
}

/// Reads a context handle argument.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn ctx_mut<'a>(ctx: *mut CobletAuthCtx) -> Result<&'a mut CobletAuthCtx, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_mut() }.ok_or_else(|| Failure::argument("ctx", "is NULL"))
}

/// Hands the launcher what the user has to do, as JSON in the format of
/// `coblet_login_prompt_json` without `schema`; `login` keeps running.
///
/// Fails with `COBLET_ERROR_KIND_INVALID_STATE` on a second call and in
/// `refresh`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `prompt_json`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_show_prompt(
    ctx: *mut CobletAuthCtx,
    prompt_json: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let prompt: PromptJson = serde_json::from_str(text(prompt_json, "prompt_json")?)
                .map_err(|error| Failure::argument("prompt_json", &error.to_string()))?;
            let prompt = prompt.into_prompt()?;
            let sender = ctx.prompt.take().ok_or_else(|| {
                Failure::new(
                    CobletErrorKind::InvalidState,
                    "the prompt was already shown",
                )
            })?;
            if let Some(auth) = &ctx.auth {
                auth.publish(&prompt);
            }
            let _ = sender.send(Some(prompt));
            Ok(CobletStatus::Ok)
        })
    }
}

/// Waits for the URI the browser was redirected to, which the launcher
/// passes to `coblet_login_submit_redirect`. On success `*out_uri`
/// receives a string to free with `coblet_string_free`.
///
/// Fails with `COBLET_ERROR_KIND_INVALID_STATE` unless the login waits for
/// a redirect (`browser_redirect`) or when called twice, and with
/// `COBLET_ERROR_KIND_CANCELLED` when the launcher cancels the login.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; `out_uri` must be writable; `out_err` must be NULL
/// or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_wait_redirect(
    ctx: *mut CobletAuthCtx,
    out_uri: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            if out_uri.is_null() {
                return Err(Failure::argument("out_uri", "is NULL"));
            }
            let receiver = ctx.redirect.take().ok_or_else(|| {
                Failure::new(
                    CobletErrorKind::InvalidState,
                    "this login does not wait for a redirect",
                )
            })?;
            let abandoned = ctx.cancel.clone();
            // The callback runs on a blocking thread, outside the runtime's
            // own tasks, so waiting here is allowed.
            let uri = ctx
                .runtime
                .block_on(abandoned.run_until_cancelled(receiver.recv()))
                .ok_or_else(|| Failure::from(AuthError::Cancelled))??;
            put(out_uri, CobletString::new(&uri), "out_uri")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Reports whether the launcher cancelled or dropped the login, or stopped
/// waiting for the refresh; false for a NULL handle.
///
/// # Safety
///
/// `ctx` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_is_cancelled(ctx: *mut CobletAuthCtx) -> bool {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { ctx.as_ref() }.is_some_and(|ctx| ctx.cancel.is_cancelled())
}

/// Sends an HTTP request through the library's transport and waits for
/// the answer; any status comes back as a response.
///
/// `request_json` is `{"method","url","headers":[[name,value]]}`; `body`
/// may be NULL. On success `*out_response_json` receives
/// `{"schema":1,"status","headers":[[name,value]]}` to free with
/// `coblet_string_free`, and `*out_body` the body, at most 1 MiB, to free
/// with `coblet_bytes_free`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; `request_json` must be NULL or NUL-terminated;
/// `body` must be NULL or readable for `body_len` bytes; the out-parameters
/// must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_send(
    ctx: *mut CobletAuthCtx,
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

/// Reads a secret saved for the account, such as `refresh_token`, in
/// `refresh`. On success `*out_found` tells whether it exists and
/// `*out_value` receives it, or an empty value, to free with
/// `coblet_string_free`.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; `name` must be NULL or NUL-terminated; the
/// out-parameters must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_secret(
    ctx: *mut CobletAuthCtx,
    name: *const c_char,
    out_found: *mut bool,
    out_value: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let name = text(name, "name")?.to_owned();
            if out_found.is_null() || out_value.is_null() {
                return Err(Failure::argument("out_found or out_value", "is NULL"));
            }
            let secret = ctx.secret_call(|answer| SecretCall::Get(name, answer))?;
            put(out_found, secret.is_some(), "out_found")?;
            let value = secret.map_or_else(CobletString::empty, |secret| {
                CobletString::new(secret.expose())
            });
            put(out_value, value, "out_value")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Saves a secret for the account at once in `refresh`, so a rotated
/// token survives a failure later in the refresh.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback, used on that
/// callback's thread; strings must be NULL or NUL-terminated; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_put_secret(
    ctx: *mut CobletAuthCtx,
    name: *const c_char,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let ctx = ctx_mut(ctx)?;
            let (name, value) = (text(name, "name")?.to_owned(), text(value, "value")?);
            let secret = Secret::new(value);
            ctx.secret_call(|answer| SecretCall::Put(name, secret, answer))?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Hands the answer of the running auth callback to the library, which
/// copies it.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `json` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_set_result(
    ctx: *mut CobletAuthCtx,
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

/// Gives the kind and reason of the failure the running auth callback is
/// about to return false for; `COBLET_ERROR_KIND_AUTH` means the user has
/// to sign in again.
///
/// # Safety
///
/// `ctx` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_ctx_fail(
    ctx: *mut CobletAuthCtx,
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

/// Adds a way of signing in written in C; start a login with
/// `coblet_login_begin_with`.
///
/// `flags` is 0 or `COBLET_AUTH_REPLACE`. The library owns the table's
/// `user_data` from this call on, even when it fails. A table without its
/// callbacks, a malformed ID or an unknown flag fails with
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
pub unsafe extern "C" fn coblet_builder_add_auth_method(
    builder: *mut CobletBuilder,
    method: CobletAuthMethod,
    flags: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table's strings; the adapter owns
    // the user data from here on.
    let adapter = unsafe { ForeignAuthMethod::new(&method) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            if flags & !COBLET_AUTH_REPLACE != 0 {
                return Err(Failure::argument(
                    "flags",
                    &format!("unknown flags {flags:#x}"),
                ));
            }
            let method: Arc<dyn coblet::auth::AuthMethod> = Arc::new(adapter?);
            builder
                .auth_methods
                .push((method, flags & COBLET_AUTH_REPLACE != 0));
            Ok(CobletStatus::Ok)
        })
    }
}

/// Lists the auth methods, such as `coblet:offline`, `coblet:microsoft`
/// and those added with `coblet_builder_add_auth_method`, as JSON:
/// `{"schema":1,"methods":[{"id","name"}]}`. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_auth_methods_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = handle(coblet)?;
            let methods: Vec<_> = handle
                .inner
                .auth_methods()
                .iter()
                .map(|method| extension_json(method.as_ref()))
                .collect();
            put_json(out_json, serde_json::json!({ "methods": methods }))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::ptr;

    use coblet::launch_command::LaunchAuth;

    use super::{
        CobletAuthCtx, CobletAuthMethod, coblet_auth_ctx_fail, coblet_auth_ctx_put_secret,
        coblet_auth_ctx_secret, coblet_auth_ctx_set_result, coblet_auth_ctx_show_prompt,
        coblet_auth_ctx_wait_redirect, coblet_auth_methods_json, coblet_builder_add_auth_method,
    };
    use crate::accounts::{
        CobletSkinModel, coblet_account_refresh, coblet_account_set_cape,
        coblet_account_upload_skin,
    };
    use crate::appearance::{
        COBLET_APPEARANCE_CAN_RESET_SKIN, COBLET_APPEARANCE_CAN_UPLOAD_PNG, CobletAppearanceCtx,
        CobletAppearanceService, coblet_appearance_capabilities_json,
        coblet_appearance_ctx_access_token, coblet_appearance_ctx_fail, coblet_appearance_ctx_png,
        coblet_appearance_ctx_set_result, coblet_builder_add_appearance_service,
    };
    use crate::builder::{CobletBuilder, coblet_builder_build, coblet_builder_new};
    use crate::context::{Coblet, coblet_error_free, coblet_free, coblet_string_free};
    use crate::login::{
        CobletLogin, coblet_login_begin_with, coblet_login_finish, coblet_login_free,
        coblet_login_prompt_json, coblet_login_submit_redirect,
    };
    use crate::types::{CobletError, CobletErrorKind, CobletStatus, CobletString};

    const PLAYER: &str = r#"{"name":"Alex","uuid":"0f5c5f6b8a7d4e2fb1d3a8c9e0f1a2b3"}"#;

    fn c(text: &str) -> CString {
        CString::new(text).unwrap()
    }

    fn json(value: &CStr) -> serde_json::Value {
        serde_json::from_str(value.to_str().unwrap()).unwrap()
    }

    /// Takes a library string as JSON and frees it.
    fn take(out: &mut CobletString) -> serde_json::Value {
        let value = std::mem::replace(out, CobletString::empty());
        // SAFETY: a string the library just handed out.
        let parsed = json(unsafe { CStr::from_ptr(value.ptr) });
        // SAFETY: freed once.
        unsafe { coblet_string_free(value) };
        parsed
    }

    /// Takes an error's kind and frees it.
    fn kind(error: *mut CobletError) -> CobletErrorKind {
        // SAFETY: an error the library just handed out.
        let kind = unsafe { (*error).kind };
        // SAFETY: freed once.
        unsafe { coblet_error_free(error) };
        kind
    }

    fn answer(ctx: *mut CobletAuthCtx, value: &serde_json::Value) -> bool {
        let text = c(&value.to_string());
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_auth_ctx_set_result(ctx, text.as_ptr()) == CobletStatus::Ok }
    }

    /// Signs in with the `server` param; waits for a redirect when asked
    /// and fails with Auth for `"fail": "yes"`.
    unsafe extern "C" fn login(
        _user_data: *mut c_void,
        ctx: *mut CobletAuthCtx,
        request: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated JSON.
        let request = json(unsafe { CStr::from_ptr(request) });
        let params = &request["params"];
        if params["fail"] == "yes" {
            let message = c("wrong password");
            // SAFETY: the running callback's handle and a valid string.
            unsafe { coblet_auth_ctx_fail(ctx, CobletErrorKind::Auth, message.as_ptr()) };
            return false;
        }
        let mut data = serde_json::json!({});
        if let Some(server) = params["server"].as_str() {
            data["server"] = server.into();
        }
        if request["interaction"]["type"] == "browser_redirect" {
            let prompt = c(
                r#"{"type":"open_url","url":"https://acme.test/authorize","redirect_uri":"acme://done"}"#,
            );
            let mut uri = CobletString::empty();
            // SAFETY: the running callback's handle, valid strings and outputs.
            unsafe {
                assert_eq!(
                    coblet_auth_ctx_show_prompt(ctx, prompt.as_ptr(), ptr::null_mut()),
                    CobletStatus::Ok
                );
                assert_eq!(
                    coblet_auth_ctx_wait_redirect(ctx, &raw mut uri, ptr::null_mut()),
                    CobletStatus::Ok
                );
                data["redirect"] = CStr::from_ptr(uri.ptr).to_str().unwrap().into();
                coblet_string_free(uri);
            }
        }
        answer(
            ctx,
            &serde_json::json!({
                "player": serde_json::from_str::<serde_json::Value>(PLAYER).unwrap(),
                "state": "ready",
                "data": data,
                "secrets": request["secrets"],
            }),
        )
    }

    /// Rotates the `token` secret and launches with it.
    unsafe extern "C" fn refresh(
        _user_data: *mut c_void,
        ctx: *mut CobletAuthCtx,
        request: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated JSON.
        let request = json(unsafe { CStr::from_ptr(request) });
        let name = c("token");
        let (mut found, mut value) = (false, CobletString::empty());
        // SAFETY: the running callback's handle, valid strings and outputs.
        let token = unsafe {
            coblet_auth_ctx_secret(
                ctx,
                name.as_ptr(),
                &raw mut found,
                &raw mut value,
                ptr::null_mut(),
            );
            let token = format!("{}+", CStr::from_ptr(value.ptr).to_str().unwrap());
            coblet_string_free(value);
            let rotated = c(&token);
            coblet_auth_ctx_put_secret(ctx, name.as_ptr(), rotated.as_ptr(), ptr::null_mut());
            token
        };
        assert!(found);
        let account = &request["account"];
        answer(
            ctx,
            &serde_json::json!({
                "change": { "player": account["player"], "state": "ready", "data": account["data"] },
                "session": {
                    "player_name": "Alex",
                    "uuid": "0f5c5f6b8a7d4e2fb1d3a8c9e0f1a2b3",
                    "access_token": token,
                    "user_type": "mojang",
                    "jvm_args": ["-Dacme.auth=1"],
                },
            }),
        )
    }

    /// Keeps one skin in memory; accepts tokens renewed from `t1`.
    unsafe extern "C" fn skins(
        _user_data: *mut c_void,
        ctx: *mut CobletAppearanceCtx,
        request: *const c_char,
    ) -> bool {
        // SAFETY: the library passes NUL-terminated JSON.
        let request = json(unsafe { CStr::from_ptr(request) });
        let mut token = CobletString::empty();
        let (mut png, mut len) = (ptr::null(), 0);
        // SAFETY: the running callback's handle and valid outputs.
        let usable = unsafe {
            coblet_appearance_ctx_access_token(ctx, &raw mut token, ptr::null_mut());
            let renewed = CStr::from_ptr(token.ptr).to_bytes().starts_with(b"t1+");
            coblet_string_free(token);
            renewed
                && coblet_appearance_ctx_png(ctx, &raw mut png, &raw mut len, ptr::null_mut())
                    == CobletStatus::Ok
        };
        if !usable {
            let message = c("unexpected request");
            // SAFETY: the running callback's handle and a valid string.
            unsafe { coblet_appearance_ctx_fail(ctx, CobletErrorKind::Other, message.as_ptr()) };
            return false;
        }
        let mut player: serde_json::Value = serde_json::from_str(PLAYER).unwrap();
        player["skins"] = serde_json::json!([{
            "id": format!("skin-{len}"),
            "url": "https://acme.test/skin.png",
            "model": request["model"],
            "active": true,
        }]);
        let text = c(&player.to_string());
        // SAFETY: the running callback's handle and a valid string.
        unsafe { coblet_appearance_ctx_set_result(ctx, text.as_ptr()) == CobletStatus::Ok }
    }

    fn coblet(dir: &tempfile::TempDir) -> *mut Coblet {
        let (root, agent) = (c(dir.path().to_str().unwrap()), c("coblet-ffi-test/1.0"));
        let (id, name, service) = (c("acme:login"), c("Acme"), c("acme:skins"));
        let (skins_id, skins_name) = (c("acme:skins"), c("Acme Skins"));
        let mut builder: *mut CobletBuilder = ptr::null_mut();
        let mut coblet: *mut Coblet = ptr::null_mut();
        // SAFETY: valid strings, tables and outputs.
        unsafe {
            coblet_builder_new(
                root.as_ptr(),
                agent.as_ptr(),
                &raw mut builder,
                ptr::null_mut(),
            );
            let method = CobletAuthMethod {
                user_data: ptr::null_mut(),
                id: id.as_ptr(),
                name: name.as_ptr(),
                appearance_service: service.as_ptr(),
                login: Some(login),
                refresh: Some(refresh),
                free: None,
            };
            assert_eq!(
                coblet_builder_add_auth_method(builder, method, 0, ptr::null_mut()),
                CobletStatus::Ok
            );
            let service = CobletAppearanceService {
                user_data: ptr::null_mut(),
                id: skins_id.as_ptr(),
                name: skins_name.as_ptr(),
                capabilities: COBLET_APPEARANCE_CAN_UPLOAD_PNG | COBLET_APPEARANCE_CAN_RESET_SKIN,
                call: Some(skins),
                free: None,
            };
            assert_eq!(
                coblet_builder_add_appearance_service(builder, service, 0, ptr::null_mut()),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_build(builder, &raw mut coblet, ptr::null_mut()),
                CobletStatus::Ok
            );
        }
        coblet
    }

    fn begin(coblet: *mut Coblet, options: &serde_json::Value) -> *mut CobletLogin {
        let (method, options) = (c("acme:login"), c(&options.to_string()));
        let mut login = ptr::null_mut();
        // SAFETY: a live handle, valid strings and outputs.
        let status = unsafe {
            coblet_login_begin_with(
                coblet,
                method.as_ptr(),
                options.as_ptr(),
                &raw mut login,
                ptr::null_mut(),
            )
        };
        assert_eq!(status, CobletStatus::Ok);
        login
    }

    fn finish(login: *mut CobletLogin) -> Result<serde_json::Value, CobletErrorKind> {
        let (mut out, mut error) = (CobletString::empty(), ptr::null_mut());
        // SAFETY: a live login and valid outputs; freed once.
        let status = unsafe { coblet_login_finish(login, &raw mut out, &raw mut error) };
        // SAFETY: freed once.
        unsafe { coblet_login_free(login) };
        if status == CobletStatus::Ok {
            Ok(take(&mut out))
        } else {
            Err(kind(error))
        }
    }

    #[test]
    fn c_methods_sign_in_refresh_and_launch_with_their_session() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(&dir);
        let mut out = CobletString::empty();
        // SAFETY: a live handle and a valid output.
        unsafe { coblet_auth_methods_json(coblet, &raw mut out, ptr::null_mut()) };
        let methods = take(&mut out);
        assert!(
            methods["methods"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == "acme:login")
        );

        let options = serde_json::json!({
            "params": { "server": "https://acme.test" },
            "secrets": { "token": "t1" },
        });
        let account = finish(begin(coblet, &options)).unwrap();
        assert_eq!(account["data"]["server"], "https://acme.test");
        let id = c(account["id"].as_str().unwrap());
        // SAFETY: a live handle, a valid string and output.
        let status =
            unsafe { coblet_account_refresh(coblet, id.as_ptr(), &raw mut out, ptr::null_mut()) };
        assert_eq!(status, CobletStatus::Ok);
        take(&mut out);
        // SAFETY: a live handle.
        let handle = unsafe { &*coblet };
        let account_id = coblet::auth::AccountId::new(account["id"].as_str().unwrap()).unwrap();
        let session = handle
            .runtime
            .block_on(handle.inner.session(&account_id))
            .unwrap();
        assert_eq!(session.jvm_args, ["-Dacme.auth=1"]);
        let LaunchAuth::Online(online) = session.auth else {
            panic!("expected an online session");
        };
        assert_eq!(online.user_type.as_deref(), Some("mojang"));
        assert_eq!(online.access_token.expose(), "t1++");
        // SAFETY: the handle is freed once.
        unsafe { coblet_free(coblet) };
    }

    #[test]
    fn c_methods_wait_for_redirects_and_report_failures() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(&dir);
        let options = serde_json::json!({
            "interaction": { "type": "browser_redirect", "redirect_uri": "acme://done" },
        });
        let login = begin(coblet, &options);
        let mut out = CobletString::empty();
        let uri = c("acme://done?code=42");
        // SAFETY: a live login, valid strings and outputs.
        unsafe {
            coblet_login_prompt_json(login, &raw mut out, ptr::null_mut());
            assert_eq!(take(&mut out)["type"], "open_url");
            coblet_login_submit_redirect(login, uri.as_ptr(), ptr::null_mut());
        }
        let account = finish(login).unwrap();
        assert_eq!(account["data"]["redirect"], "acme://done?code=42");

        let failing = serde_json::json!({ "params": { "fail": "yes" } });
        assert_eq!(finish(begin(coblet, &failing)), Err(CobletErrorKind::Auth));
        // SAFETY: the handle is freed once.
        unsafe { coblet_free(coblet) };
    }

    #[test]
    fn c_appearance_services_get_only_what_they_can_do() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(&dir);
        let options = serde_json::json!({ "secrets": { "token": "t1" } });
        let account = finish(begin(coblet, &options)).unwrap();
        let id = c(account["id"].as_str().unwrap());
        let mut out = CobletString::empty();
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&64_u32.to_be_bytes());
        png.extend_from_slice(&64_u32.to_be_bytes());
        let cape = c("migrator");
        let mut error = ptr::null_mut();
        // SAFETY: a live handle, valid strings, bytes and outputs.
        unsafe {
            coblet_appearance_capabilities_json(coblet, id.as_ptr(), &raw mut out, ptr::null_mut());
            let allowed = take(&mut out);
            assert_eq!(
                (
                    allowed["upload_png"].as_bool(),
                    allowed["choose_cape"].as_bool()
                ),
                (Some(true), Some(false))
            );
            let status = coblet_account_upload_skin(
                coblet,
                id.as_ptr(),
                png.as_ptr(),
                png.len(),
                CobletSkinModel::Slim,
                &raw mut out,
                ptr::null_mut(),
            );
            assert_eq!(status, CobletStatus::Ok);
            let worn = take(&mut out);
            assert_eq!(worn["skins"][0]["model"], "slim");
            assert_eq!(worn["skins"][0]["id"], format!("skin-{}", png.len()));
            let status = coblet_account_set_cape(
                coblet,
                id.as_ptr(),
                cape.as_ptr(),
                &raw mut out,
                &raw mut error,
            );
            assert_eq!(status, CobletStatus::Error);
            assert_eq!(kind(error), CobletErrorKind::Unsupported);
            coblet_free(coblet);
        }
    }
}
