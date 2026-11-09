use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::SystemTime;

use coblet::auth::{
    Account, AccountSecrets, AccountUpdate, AuthCtx, AuthError, AuthFlow, CallbackReceiver,
    Credentials, Interaction, LaunchSession, LoginOptions, Redirect, Refreshed, callback_channel,
};
use coblet::launch_command::{Credential, LaunchAuth, OnlineSession};
use coblet::runtime::{
    AuthPrompt, BoxFuture, CancellationToken, Extension, HttpUrl, Secret, Transport,
};
use coblet::types::{ExtensionId, PlayerName};
use tokio::sync::oneshot;

use crate::http::{HttpRequest, HttpResponse, send};
use crate::java::extension_id;
use crate::{
    AccountInfo, AccountState, CobletError, ErrorKind, LoginInteraction, LoginPrompt, PlayerInfo,
};

/// Way of signing in written in Swift, Kotlin or Python, such as a login
/// server that follows the authlib-injector specification.
///
/// `login` and `refresh` run on a worker thread and may block. Register it
/// with `CobletBuilder.add_auth_method` and start a login with
/// `Coblet.begin_login`.
#[uniffi::export(foreign)]
pub trait AuthMethod: Send + Sync {
    /// Returns the ID, such as `acme:login`; read once at registration.
    fn id(&self) -> String;

    /// Returns the name shown to users; read once at registration.
    fn display_name(&self) -> String;

    /// Returns the ID of the appearance service for its accounts' skins
    /// and capes, if any; read once at registration.
    fn appearance_service(&self) -> Option<String>;

    /// Signs in and returns the account data to save.
    ///
    /// Call `ctx.show_prompt` once when the user has something to do, such
    /// as entering a code; the launcher gets the prompt while `login` keeps
    /// running. A login that needs no user returns without a prompt.
    ///
    /// # Errors
    ///
    /// An error ends the login; nothing is saved.
    fn login(
        &self,
        ctx: Arc<LoginContext>,
        request: LoginRequest,
    ) -> Result<AccountChange, CobletError>;

    /// Returns a session to launch with, renewing tokens when `mode` asks
    /// for it or when they are about to expire. Secrets written through
    /// `ctx.put_secret` are saved at once.
    ///
    /// # Errors
    ///
    /// An error stops the launch or account call that needed the session;
    /// use `ErrorKind.Auth` when the user has to sign in again.
    fn refresh(
        &self,
        ctx: Arc<RefreshContext>,
        account: AccountInfo,
        mode: RefreshMode,
    ) -> Result<RefreshResult, CobletError>;
}

/// What a login was started with.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LoginRequest {
    /// Name or e-mail address to pre-fill, if given.
    pub login_hint: Option<String>,
    /// How the user signs in; `BrowserRedirect` lets the login wait for
    /// the redirect with `ctx.wait_redirect`.
    pub interaction: LoginInteraction,
    /// Inputs the method defines, such as the address of a login server.
    pub params: HashMap<String, String>,
    /// Sensitive inputs the method defines, such as a password.
    pub secrets: HashMap<String, String>,
}

/// When `refresh` talks to the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RefreshMode {
    /// Reuse the current session unless it expires soon.
    IfNeeded,
    /// Renew the session and the profile now.
    Force,
    /// Read the license and profile again; methods without licenses treat
    /// it like `IfNeeded`.
    Recheck,
}

impl From<coblet::auth::RefreshMode> for RefreshMode {
    fn from(mode: coblet::auth::RefreshMode) -> Self {
        match mode {
            coblet::auth::RefreshMode::Force => Self::Force,
            coblet::auth::RefreshMode::Recheck => Self::Recheck,
            coblet::auth::RefreshMode::IfNeeded => Self::IfNeeded,
        }
    }
}

/// Account data a login or refresh produced.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AccountChange {
    /// Player profile, if the account has one.
    pub player: Option<PlayerInfo>,
    /// New state.
    pub state: AccountState,
    /// When the game session expires, if known.
    pub session_expires_at: Option<SystemTime>,
    /// Non-sensitive values to keep, replacing the previous ones.
    pub data: HashMap<String, String>,
    /// Secrets to save, by names of `a-z 0-9 _ -`; other saved secrets stay.
    pub secrets: HashMap<String, String>,
}

/// Identity to launch the game with.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SessionInfo {
    /// Player name.
    pub player_name: String,
    /// Profile UUID; ignored for offline sessions.
    pub uuid: String,
    /// Access token; `None` launches offline.
    pub access_token: Option<String>,
    /// Xbox user ID, if any.
    pub xuid: Option<String>,
    /// Client ID passed to the game, if any.
    pub client_id: Option<String>,
    /// Value of `${user_type}`, such as `mojang`; `msa` when `None`.
    pub user_type: Option<String>,
    /// JVM arguments the method needs, such as a `-javaagent`.
    pub jvm_args: Vec<String>,
    /// When the access token expires, if known.
    pub expires_at: Option<SystemTime>,
}

/// Result of `AuthMethod.refresh`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RefreshResult {
    /// Account data to save.
    pub change: AccountChange,
    /// Session to launch with, or `None` when the account cannot play.
    pub session: Option<SessionInfo>,
}

/// Auth method known to a [`Coblet`](crate::Coblet).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AuthMethodInfo {
    /// ID, such as `coblet:microsoft`.
    pub id: String,
    /// Name shown to users.
    pub name: String,
}

/// The running login, valid until `login` returns.
#[derive(uniffi::Object)]
pub struct LoginContext {
    runtime: tokio::runtime::Handle,
    transport: Arc<dyn Transport>,
    auth: AuthCtx,
    /// Cancelled when the launcher cancels or drops the login.
    abandoned: CancellationToken,
    prompt: Mutex<Option<oneshot::Sender<Option<AuthPrompt>>>>,
    redirect: Mutex<Option<CallbackReceiver>>,
}

impl std::fmt::Debug for LoginContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginContext").finish_non_exhaustive()
    }
}

impl LoginContext {
    /// Tells `begin` that `login` returned without a prompt.
    fn returned(&self) {
        if let Some(sender) = self
            .prompt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(None);
        }
    }
}

#[uniffi::export]
impl LoginContext {
    /// Hands the launcher what the user has to do; call it once.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidState` on a second call and `InvalidArgument`
    /// for a malformed address.
    pub fn show_prompt(&self, prompt: LoginPrompt) -> Result<(), CobletError> {
        let prompt = auth_prompt(prompt)?;
        let sender = self
            .prompt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                CobletError::new(ErrorKind::InvalidState, "the prompt was already shown")
            })?;
        self.auth.publish(&prompt);
        let _ = sender.send(Some(prompt));
        Ok(())
    }

    /// Waits for the URI the browser was redirected to, which the launcher
    /// passes to `LoginFlow.submit_redirect`.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidState` unless the login was started with
    /// `LoginInteraction.BrowserRedirect` or when called twice, and with
    /// `Cancelled` when the launcher cancels the login.
    pub fn wait_redirect(&self) -> Result<String, CobletError> {
        let receiver = self
            .redirect
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                CobletError::new(
                    ErrorKind::InvalidState,
                    "this login does not wait for a redirect",
                )
            })?;
        let abandoned = self.abandoned.clone();
        let (sender, answer) = mpsc::sync_channel(1);
        self.runtime.spawn(async move {
            let _ = sender.send(abandoned.run_until_cancelled(receiver.recv()).await);
        });
        match answer.recv() {
            Ok(Some(Ok(uri))) => Ok(uri),
            Ok(Some(Err(error))) => Err(CobletError::from(coblet::Error::Auth(error))),
            _ => Err(CobletError::new(
                ErrorKind::Cancelled,
                "the login was cancelled",
            )),
        }
    }

    /// Reports whether the launcher cancelled or dropped the login.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.abandoned.is_cancelled()
    }

    /// Sends an HTTP request through coblet's transport; any status comes
    /// back as a response.
    ///
    /// # Errors
    ///
    /// Fails for a malformed request and when the server cannot be reached.
    pub fn send(&self, request: HttpRequest) -> Result<HttpResponse, CobletError> {
        send(&self.runtime, &self.transport, request)
    }
}

/// Secret call a foreign refresh makes; the refresh future runs it, since
/// it holds the account's secrets.
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

/// The running refresh of one account, valid until `refresh` returns.
#[derive(uniffi::Object)]
pub struct RefreshContext {
    runtime: tokio::runtime::Handle,
    transport: Arc<dyn Transport>,
    cancel: CancellationToken,
    calls: tokio::sync::mpsc::UnboundedSender<SecretCall>,
}

impl std::fmt::Debug for RefreshContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshContext").finish_non_exhaustive()
    }
}

impl RefreshContext {
    fn call<T>(
        &self,
        call: impl FnOnce(mpsc::SyncSender<Result<T, AuthError>>) -> SecretCall,
    ) -> Result<T, CobletError> {
        let ended = || CobletError::new(ErrorKind::InvalidState, "the refresh has ended");
        let (sender, answer) = mpsc::sync_channel(1);
        self.calls.send(call(sender)).map_err(|_| ended())?;
        answer
            .recv()
            .map_err(|_| ended())?
            .map_err(|error| CobletError::from(coblet::Error::Auth(error)))
    }
}

#[uniffi::export]
impl RefreshContext {
    /// Reads a secret saved for the account, such as `refresh_token`.
    ///
    /// # Errors
    ///
    /// Fails for a malformed name and when the secret store fails.
    pub fn secret(&self, name: String) -> Result<Option<String>, CobletError> {
        let secret = self.call(|answer| SecretCall::Get(name, answer))?;
        Ok(secret.map(|secret| secret.expose().to_owned()))
    }

    /// Saves a secret for the account at once, so a rotated token survives
    /// a failure later in the refresh.
    ///
    /// # Errors
    ///
    /// Fails for a malformed name and when the secret store fails.
    pub fn put_secret(&self, name: String, value: String) -> Result<(), CobletError> {
        self.call(|answer| SecretCall::Put(name, Secret::new(value), answer))
    }

    /// Reports whether the launcher stopped waiting for the refresh.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Sends an HTTP request through coblet's transport; any status comes
    /// back as a response.
    ///
    /// # Errors
    ///
    /// Fails for a malformed request and when the server cannot be reached.
    pub fn send(&self, request: HttpRequest) -> Result<HttpResponse, CobletError> {
        send(&self.runtime, &self.transport, request)
    }
}

fn auth_prompt(prompt: LoginPrompt) -> Result<AuthPrompt, CobletError> {
    let url = |url: String| {
        HttpUrl::new(url)
            .map_err(|error| CobletError::new(ErrorKind::InvalidArgument, error.to_string()))
    };
    Ok(match prompt {
        LoginPrompt::DeviceCode {
            user_code,
            verification_uri,
            expires_at,
            message,
        } => AuthPrompt::DeviceCode {
            user_code,
            verification_uri: url(verification_uri)?,
            expires_at,
            message,
        },
        LoginPrompt::OpenUrl {
            url: page,
            redirect_uri,
        } => AuthPrompt::OpenUrl {
            url: url(page)?,
            redirect_uri,
        },
        LoginPrompt::Nothing => AuthPrompt::Nothing,
    })
}

fn interaction(interaction: &Interaction) -> LoginInteraction {
    match interaction {
        Interaction::Browser(Redirect::External { redirect_uri }) => {
            LoginInteraction::BrowserRedirect {
                redirect_uri: redirect_uri.clone(),
            }
        }
        Interaction::Browser(Redirect::Loopback { host, port, path }) => {
            LoginInteraction::BrowserLoopback {
                host: host.clone(),
                port: *port,
                path: path.clone(),
            }
        }
        _ => LoginInteraction::DeviceCode,
    }
}

/// Turns a foreign error into an auth error; `Auth` means the user has to
/// sign in again.
fn failed(error: &CobletError) -> AuthError {
    match error.kind() {
        ErrorKind::Cancelled => AuthError::Cancelled,
        ErrorKind::Auth => AuthError::InvalidGrant,
        ErrorKind::NoLicense => AuthError::NoLicense,
        ErrorKind::NoProfile => AuthError::NoProfile,
        _ => AuthError::Failed(error.to_string()),
    }
}

fn update(change: AccountChange) -> Result<AccountUpdate, CobletError> {
    let mut update = AccountUpdate::new(change.state.into());
    if let Some(player) = change.player {
        update = update.with_profile(player.try_into()?);
    }
    update.session_expires_at = change.session_expires_at;
    update.data = change.data.into_iter().collect();
    let mut secrets = Credentials::new();
    for (name, value) in change.secrets {
        secrets.insert(name, Secret::new(value));
    }
    update.secrets = secrets;
    Ok(update)
}

fn session(info: SessionInfo) -> Result<LaunchSession, CobletError> {
    let name = PlayerName::new(info.player_name)
        .map_err(|error| CobletError::new(ErrorKind::InvalidArgument, error.to_string()))?;
    let auth = match info.access_token {
        None => LaunchAuth::offline(name),
        Some(token) => {
            let mut online = OnlineSession::new(name, info.uuid, Credential::new(token));
            online.xuid = info.xuid.map(Credential::new);
            online.client_id = info.client_id.map(Credential::new);
            online.user_type = info.user_type;
            LaunchAuth::online(online)
        }
    };
    let mut session = LaunchSession::new(auth).with_jvm_args(info.jvm_args);
    session.expires_at = info.expires_at;
    Ok(session)
}

/// Runs a foreign auth method as a Rust one.
pub(crate) struct ForeignAuthMethod {
    id: ExtensionId,
    display_name: String,
    appearance: Option<ExtensionId>,
    inner: Arc<dyn AuthMethod>,
}

impl ForeignAuthMethod {
    pub(crate) fn new(inner: Arc<dyn AuthMethod>) -> Result<Self, CobletError> {
        let appearance = inner
            .appearance_service()
            .map(|id| extension_id("appearance service", &id))
            .transpose()?;
        Ok(Self {
            id: extension_id("auth method", &inner.id())?,
            display_name: inner.display_name(),
            appearance,
            inner,
        })
    }
}

impl Extension for ForeignAuthMethod {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
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
            let (callback, receiver) = if wants_redirect {
                let (sender, receiver) = callback_channel();
                (Some(sender), Some(receiver))
            } else {
                (None, None)
            };
            let context = Arc::new(LoginContext {
                runtime: tokio::runtime::Handle::current(),
                transport: Arc::clone(ctx.transport()),
                auth: ctx.clone(),
                abandoned: abandoned.clone(),
                prompt: Mutex::new(Some(prompt_sender)),
                redirect: Mutex::new(receiver),
            });
            let request = LoginRequest {
                login_hint: options.login_hint.clone(),
                interaction: interaction(&options.interaction),
                params: options.params.clone().into_iter().collect(),
                secrets: options
                    .secrets
                    .iter()
                    .map(|(name, secret)| (name.to_owned(), secret.expose().to_owned()))
                    .collect(),
            };
            let inner = Arc::clone(&self.inner);
            tokio::task::spawn_blocking(move || {
                let result = inner.login(Arc::clone(&context), request);
                context.returned();
                let _ = done_sender.send(result);
            });
            // Dropping the flow, also unpolled, tells the login to stop.
            let guard = abandoned.drop_guard();
            let shown = prompt.await.ok().flatten().unwrap_or(AuthPrompt::Nothing);
            let outcome = async move {
                let _guard = guard;
                let change = done
                    .await
                    .map_err(|_| AuthError::Failed("the login stopped".to_owned()))?
                    .map_err(|error| failed(&error))?;
                update(change).map_err(|error| failed(&error))
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
        mode: coblet::auth::RefreshMode,
    ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
        Box::pin(async move {
            let (calls, mut requests) = tokio::sync::mpsc::unbounded_channel();
            let context = Arc::new(RefreshContext {
                runtime: tokio::runtime::Handle::current(),
                transport: Arc::clone(ctx.transport()),
                cancel: ctx.cancel().clone(),
                calls,
            });
            let inner = Arc::clone(&self.inner);
            let info = AccountInfo::from(account);
            let mut task =
                tokio::task::spawn_blocking(move || inner.refresh(context, info, mode.into()));
            // Secret calls are served here, where the secrets live, until
            // the foreign refresh returns.
            let result = loop {
                tokio::select! {
                    joined = &mut task => break joined,
                    Some(call) = requests.recv() => call.run(secrets).await,
                }
            }
            .map_err(|_| AuthError::Failed("the refresh stopped".to_owned()))?
            .map_err(|error| failed(&error))?;
            let update = update(result.change).map_err(|error| failed(&error))?;
            let session = result
                .session
                .map(session)
                .transpose()
                .map_err(|error| failed(&error))?;
            Ok(Refreshed::new(update, session))
        })
    }

    fn appearance_service(&self) -> Option<ExtensionId> {
        self.appearance.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, SystemTime};

    use coblet::launch_command::LaunchAuth;

    use super::{
        AccountChange, AuthMethod, LoginContext, LoginRequest, RefreshContext, RefreshMode,
        RefreshResult, SessionInfo,
    };
    use crate::{
        AccountInfo, AccountState, Coblet, CobletBuilder, CobletError, ErrorKind, LoginInteraction,
        LoginPrompt, PlayerInfo,
    };

    const UUID: &str = "0f5c5f6b8a7d4e2fb1d3a8c9e0f1a2b3";

    #[derive(Clone, Copy)]
    enum Script {
        /// Shows a device code, then waits for the gate.
        Prompted,
        /// Returns at once.
        Silent,
        /// Waits for a browser redirect.
        Redirect,
        /// Shows the prompt twice.
        Twice,
        /// Fails as if the password were wrong.
        Rejected,
    }

    struct Site {
        script: Script,
        gate: Mutex<Option<mpsc::Receiver<()>>>,
        seen: Mutex<Vec<String>>,
    }

    impl Site {
        fn new(script: Script) -> Arc<Self> {
            Arc::new(Self {
                script,
                gate: Mutex::new(None),
                seen: Mutex::new(Vec::new()),
            })
        }

        fn note(&self, line: impl Into<String>) {
            self.seen.lock().unwrap().push(line.into());
        }
    }

    fn ready(data: HashMap<String, String>, secrets: HashMap<String, String>) -> AccountChange {
        AccountChange {
            player: Some(PlayerInfo {
                name: "Alex".to_owned(),
                uuid: UUID.to_owned(),
                skins: Vec::new(),
                capes: Vec::new(),
            }),
            state: AccountState::Ready,
            session_expires_at: None,
            data,
            secrets,
        }
    }

    impl AuthMethod for Site {
        fn id(&self) -> String {
            "acme:login".to_owned()
        }

        fn display_name(&self) -> String {
            "Acme".to_owned()
        }

        fn appearance_service(&self) -> Option<String> {
            None
        }

        fn login(
            &self,
            ctx: Arc<LoginContext>,
            request: LoginRequest,
        ) -> Result<AccountChange, CobletError> {
            let code = || LoginPrompt::DeviceCode {
                user_code: "ABCD".to_owned(),
                verification_uri: "https://acme.test/device".to_owned(),
                expires_at: SystemTime::now() + Duration::from_secs(600),
                message: None,
            };
            let mut data = HashMap::new();
            match self.script {
                Script::Prompted => {
                    ctx.show_prompt(code())?;
                    let gate = self.gate.lock().unwrap().take().unwrap();
                    gate.recv().unwrap();
                }
                Script::Silent => {}
                Script::Redirect => {
                    ctx.show_prompt(LoginPrompt::OpenUrl {
                        url: "https://acme.test/authorize".to_owned(),
                        redirect_uri: "acme://done".to_owned(),
                    })?;
                    match ctx.wait_redirect() {
                        Ok(uri) => {
                            data.insert("redirect".to_owned(), uri);
                        }
                        Err(error) => {
                            self.note(format!("wait: {:?}", error.kind()));
                            self.note(format!("cancelled: {}", ctx.is_cancelled()));
                            return Err(error);
                        }
                    }
                }
                Script::Twice => {
                    ctx.show_prompt(code())?;
                    let second = ctx.show_prompt(code()).unwrap_err();
                    self.note(format!("second: {:?}", second.kind()));
                }
                Script::Rejected => {
                    return Err(CobletError::new(ErrorKind::Auth, "wrong password"));
                }
            }
            self.note(format!("hint: {:?}", request.login_hint));
            data.extend(request.params);
            let secrets = request.secrets.into_iter().collect();
            Ok(ready(data, secrets))
        }

        fn refresh(
            &self,
            ctx: Arc<RefreshContext>,
            account: AccountInfo,
            mode: RefreshMode,
        ) -> Result<RefreshResult, CobletError> {
            let token = ctx.secret("token".to_owned())?.unwrap_or_default();
            ctx.put_secret("token".to_owned(), format!("{token}+"))?;
            self.note(format!("refresh {mode:?} with {token}"));
            let mut change = ready(account.data, HashMap::new());
            change.player = account.player;
            Ok(RefreshResult {
                change,
                session: Some(SessionInfo {
                    player_name: "Alex".to_owned(),
                    uuid: UUID.to_owned(),
                    access_token: Some(token),
                    xuid: None,
                    client_id: None,
                    user_type: Some("mojang".to_owned()),
                    jvm_args: vec!["-Dacme.auth=1".to_owned()],
                    expires_at: None,
                }),
            })
        }
    }

    fn coblet(dir: &tempfile::TempDir, site: &Arc<Site>) -> Arc<Coblet> {
        let builder = CobletBuilder::new(
            dir.path().to_string_lossy().into_owned(),
            "coblet-uniffi-test/1.0".to_owned(),
        )
        .unwrap();
        builder.add_auth_method(site.clone()).unwrap();
        builder.build().unwrap()
    }

    fn inputs() -> (HashMap<String, String>, HashMap<String, String>) {
        let params = HashMap::from([("server".to_owned(), "https://acme.test".to_owned())]);
        let secrets = HashMap::from([("token".to_owned(), "t1".to_owned())]);
        (params, secrets)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_prompt_comes_before_the_login_ends() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::new(Script::Prompted);
        let (open, gate) = mpsc::channel();
        *site.gate.lock().unwrap() = Some(gate);
        let coblet = coblet(&dir, &site);
        assert!(
            coblet
                .auth_methods()
                .iter()
                .any(|method| method.id == "acme:login" && method.name == "Acme")
        );
        let (params, secrets) = inputs();
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                LoginInteraction::DeviceCode,
                Some("alex@acme.test".to_owned()),
                params,
                secrets,
            )
            .await
            .unwrap();
        assert!(
            matches!(flow.prompt(), LoginPrompt::DeviceCode { user_code, .. } if user_code == "ABCD")
        );
        open.send(()).unwrap();
        let account = flow.finish().await.unwrap();
        assert_eq!(account.method, "acme:login");
        assert_eq!(account.data["server"], "https://acme.test");
        assert_eq!(
            site.seen.lock().unwrap().as_slice(),
            ["hint: Some(\"alex@acme.test\")"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_login_without_a_prompt_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::new(Script::Silent);
        let coblet = coblet(&dir, &site);
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                LoginInteraction::DeviceCode,
                None,
                HashMap::new(),
                HashMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(flow.prompt(), LoginPrompt::Nothing);
        assert!(!flow.accepts_redirect());
        let account = flow.finish().await.unwrap();
        assert_eq!(account.player.unwrap().name, "Alex");
    }

    fn redirect() -> LoginInteraction {
        LoginInteraction::BrowserRedirect {
            redirect_uri: "acme://done".to_owned(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_redirect_reaches_the_waiting_login() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::new(Script::Redirect);
        let coblet = coblet(&dir, &site);
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                redirect(),
                None,
                HashMap::new(),
                HashMap::new(),
            )
            .await
            .unwrap();
        assert!(matches!(flow.prompt(), LoginPrompt::OpenUrl { .. }));
        assert!(flow.accepts_redirect());
        flow.submit_redirect("acme://done?code=42".to_owned())
            .unwrap();
        let account = flow.finish().await.unwrap();
        assert_eq!(account.data["redirect"], "acme://done?code=42");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_stops_a_login_waiting_for_the_redirect() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::new(Script::Redirect);
        let coblet = coblet(&dir, &site);
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                redirect(),
                None,
                HashMap::new(),
                HashMap::new(),
            )
            .await
            .unwrap();
        flow.cancel();
        let error = flow.finish().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Cancelled);
        drop(flow);
        for _ in 0..200 {
            if site.seen.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            site.seen.lock().unwrap().as_slice(),
            ["wait: Cancelled", "cancelled: true"]
        );
        assert_eq!(coblet.accounts().await.unwrap().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_prompt_is_shown_once() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::new(Script::Twice);
        let coblet = coblet(&dir, &site);
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                LoginInteraction::DeviceCode,
                None,
                HashMap::new(),
                HashMap::new(),
            )
            .await
            .unwrap();
        flow.finish().await.unwrap();
        assert_eq!(site.seen.lock().unwrap()[0], "second: InvalidState");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_rejected_login_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::new(Script::Rejected);
        let coblet = coblet(&dir, &site);
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                LoginInteraction::DeviceCode,
                None,
                HashMap::new(),
                HashMap::new(),
            )
            .await
            .unwrap();
        let error = flow.finish().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Auth);
        assert_eq!(coblet.accounts().await.unwrap().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refresh_saves_secrets_and_launches_with_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::new(Script::Silent);
        let coblet = coblet(&dir, &site);
        let (params, secrets) = inputs();
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                LoginInteraction::DeviceCode,
                None,
                params,
                secrets,
            )
            .await
            .unwrap();
        let account = flow.finish().await.unwrap();
        coblet.refresh_account(account.id.clone()).await.unwrap();
        let id = crate::account::account_id(&account.id).unwrap();
        let session = coblet.inner.session(&id).await.unwrap();
        let seen = site.seen.lock().unwrap().clone();
        assert_eq!(
            seen[1..],
            ["refresh Force with t1", "refresh IfNeeded with t1+"]
        );
        assert_eq!(session.jvm_args, ["-Dacme.auth=1"]);
        let LaunchAuth::Online(online) = session.auth else {
            panic!("expected an online session");
        };
        assert_eq!(online.user_type.as_deref(), Some("mojang"));
        assert_eq!(online.access_token.expose(), "t1+");
    }

    #[tokio::test]
    async fn a_malformed_id_is_refused_when_added() {
        struct Nameless;
        impl AuthMethod for Nameless {
            fn id(&self) -> String {
                "no colon".to_owned()
            }
            fn display_name(&self) -> String {
                String::new()
            }
            fn appearance_service(&self) -> Option<String> {
                None
            }
            fn login(
                &self,
                _ctx: Arc<LoginContext>,
                _request: LoginRequest,
            ) -> Result<AccountChange, CobletError> {
                unreachable!()
            }
            fn refresh(
                &self,
                _ctx: Arc<RefreshContext>,
                _account: AccountInfo,
                _mode: RefreshMode,
            ) -> Result<RefreshResult, CobletError> {
                unreachable!()
            }
        }
        let builder =
            CobletBuilder::new("/tmp/coblet-uniffi-auth".to_owned(), "t/1".to_owned()).unwrap();
        let error = builder.add_auth_method(Arc::new(Nameless)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }
}
