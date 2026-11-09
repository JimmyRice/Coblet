use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use coblet_runtime::{
    AuthPrompt, BoxFuture, CancellationToken, Clock, Event, EventBus, Extension, Secret,
    SystemClock, Transport,
};

use crate::{
    Account, AccountSecrets, AccountState, AuthError, AuthFlow, LaunchSession, LicenseCheck,
    PlayerProfile,
};

/// Way of signing in, registered under an [`ExtensionId`](coblet_types::ExtensionId).
///
/// coblet ships `coblet:offline` and, with the `msa` feature,
/// `coblet:microsoft`. A third-party login service, such as an
/// authlib-injector compatible server, implements this trait and can add the
/// JVM arguments it needs through [`LaunchSession::jvm_args`].
///
/// Methods never write account files: [`begin`](Self::begin) and
/// [`refresh`](Self::refresh) return an [`AccountUpdate`], and the account
/// store saves it. A refresh writes rotated secrets through its
/// [`AccountSecrets`] at once, so they survive a failure later in the refresh.
pub trait AuthMethod: Extension {
    /// Starts an interactive login.
    ///
    /// The returned flow carries the prompt to show; finishing it yields the
    /// new account data.
    fn begin<'a>(
        &'a self,
        ctx: &'a AuthCtx,
        options: &'a LoginOptions,
    ) -> BoxFuture<'a, Result<AuthFlow, AuthError>>;

    /// Returns a session to launch with, renewing tokens when `mode` asks
    /// for it or when they are about to expire.
    ///
    /// `secrets` reads and writes the secrets saved for the account.
    fn refresh<'a>(
        &'a self,
        ctx: &'a AuthCtx,
        account: &'a Account,
        secrets: &'a AccountSecrets,
        mode: RefreshMode,
    ) -> BoxFuture<'a, Result<Refreshed, AuthError>>;
}

/// Services available to an auth method.
///
/// Cloning is cheap.
#[derive(Clone)]
pub struct AuthCtx {
    transport: Arc<dyn Transport>,
    clock: Arc<dyn Clock>,
    events: Option<EventBus>,
    cancel: CancellationToken,
}

impl AuthCtx {
    /// Creates a context with the system clock, no event bus and a fresh
    /// cancellation token.
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            clock: Arc::new(SystemClock),
            events: None,
            cancel: CancellationToken::new(),
        }
    }

    /// Uses this clock for expiry times.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Publishes login prompts to this bus.
    #[must_use]
    pub fn with_events(mut self, events: EventBus) -> Self {
        self.events = Some(events);
        self
    }

    /// Stops waiting flows when this token is cancelled.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Returns the transport.
    #[must_use]
    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    /// Returns the clock.
    #[must_use]
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Returns the current time of the clock.
    #[must_use]
    pub fn now(&self) -> SystemTime {
        self.clock.now()
    }

    /// Returns the cancellation token.
    #[must_use]
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Publishes a prompt as [`Event::AuthPrompt`], if an event bus is set.
    pub fn publish(&self, prompt: &AuthPrompt) {
        if let Some(events) = &self.events {
            events.publish(Event::AuthPrompt(prompt.clone()));
        }
    }
}

impl std::fmt::Debug for AuthCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCtx")
            .field("events", &self.events.is_some())
            .finish_non_exhaustive()
    }
}

/// How the user interacts with a login.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Interaction {
    /// Show a code that the user enters on another device. Suits command
    /// line tools and machines without a browser.
    #[default]
    DeviceCode,
    /// Open a sign-in page in a browser and wait for its redirect.
    Browser(Redirect),
}

/// Where the browser is sent after signing in.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Redirect {
    /// A listener on this machine receives the redirect. Needs the
    /// `loopback` feature.
    Loopback {
        /// Host in the redirect URI, such as `127.0.0.1` or `localhost`;
        /// the listener always binds to the loopback interface.
        host: String,
        /// Port, or 0 to pick a free one.
        port: u16,
        /// Path of the redirect URI, starting with `/`.
        path: String,
    },
    /// The caller receives the redirect, for example through a custom URL
    /// scheme, and hands the full URI to
    /// [`CallbackSender::submit`](crate::CallbackSender::submit).
    External {
        /// Redirect URI registered for the Client ID.
        redirect_uri: String,
    },
}

impl Redirect {
    /// Returns a loopback redirect to `http://127.0.0.1:<free port>/`.
    #[must_use]
    pub fn loopback() -> Self {
        Self::Loopback {
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/".to_owned(),
        }
    }
}

/// Options of one login.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginOptions {
    /// Name of the account: the player name for offline accounts, a
    /// suggested e-mail address for Microsoft accounts.
    pub login_hint: Option<String>,
    /// How the user signs in.
    pub interaction: Interaction,
    /// Inputs a method defines for itself, such as the address of a login
    /// server; the built-in methods ignore them.
    pub params: BTreeMap<String, String>,
    /// Sensitive inputs a method defines for itself, such as a password;
    /// the built-in methods ignore them.
    pub secrets: Credentials,
}

impl LoginOptions {
    /// Creates options with a device code login and no hint.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the login hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.login_hint = Some(hint.into());
        self
    }

    /// Sets the interaction.
    #[must_use]
    pub fn with_interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = interaction;
        self
    }

    /// Adds or replaces an input of the method.
    #[must_use]
    pub fn with_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(name.into(), value.into());
        self
    }

    /// Adds or replaces a sensitive input of the method.
    #[must_use]
    pub fn with_secret(mut self, name: impl Into<String>, secret: Secret) -> Self {
        self.secrets.insert(name, secret);
        self
    }
}

/// When [`AuthMethod::refresh`] talks to the network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RefreshMode {
    /// Reuse the current session unless it expires soon.
    #[default]
    IfNeeded,
    /// Renew the session and the profile now.
    Force,
    /// Reuse the current session unless it expires soon, but read the
    /// license and the profile again. Methods without licenses treat it
    /// like [`IfNeeded`](Self::IfNeeded).
    Recheck,
}

/// Secrets of one account, keyed by a short name such as `refresh_token`.
///
/// Names may use `a-z 0-9 _ -`; the account store turns them into
/// [`SecretKey`](coblet_runtime::SecretKey)s.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials(BTreeMap<String, Secret>);

impl Credentials {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a secret.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Secret> {
        self.0.get(name)
    }

    /// Adds or replaces a secret.
    pub fn insert(&mut self, name: impl Into<String>, secret: Secret) {
        self.0.insert(name.into(), secret);
    }

    /// Removes a secret.
    pub fn remove(&mut self, name: &str) -> Option<Secret> {
        self.0.remove(name)
    }

    /// Iterates in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Secret)> {
        self.0.iter().map(|(name, secret)| (name.as_str(), secret))
    }

    /// Returns the number of secrets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Reports whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Account data produced by a login or a refresh.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountUpdate {
    /// Player profile, if the account has one.
    pub profile: Option<PlayerProfile>,
    /// New state.
    pub state: AccountState,
    /// License read by this login or refresh. `None` keeps the previous
    /// check.
    pub license: Option<LicenseCheck>,
    /// When the game session expires, if known.
    pub session_expires_at: Option<SystemTime>,
    /// Non-sensitive values to keep, replacing the previous ones.
    pub data: BTreeMap<String, String>,
    /// Secrets to save. Names already stored are replaced; other stored
    /// secrets stay.
    pub secrets: Credentials,
}

impl AccountUpdate {
    /// Creates an update with a state and nothing else.
    #[must_use]
    pub fn new(state: AccountState) -> Self {
        Self {
            profile: None,
            state,
            license: None,
            session_expires_at: None,
            data: BTreeMap::new(),
            secrets: Credentials::new(),
        }
    }

    /// Sets the profile.
    #[must_use]
    pub fn with_profile(mut self, profile: PlayerProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Takes the profile, state, license, data and expiry of an existing account,
    /// without secrets to save.
    #[must_use]
    pub fn from_account(account: &Account) -> Self {
        Self {
            profile: account.profile.clone(),
            state: account.state,
            license: account.license,
            session_expires_at: account.session_expires_at,
            data: account.data.clone(),
            secrets: Credentials::new(),
        }
    }
}

/// Result of [`AuthMethod::refresh`].
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Refreshed {
    /// Account data to save.
    pub update: AccountUpdate,
    /// Session to launch with, or `None` when the account cannot play
    /// ([`AccountState::NoLicense`] or [`AccountState::NoProfile`]).
    pub session: Option<LaunchSession>,
}

impl Refreshed {
    /// Combines an update and a session.
    #[must_use]
    pub fn new(update: AccountUpdate, session: Option<LaunchSession>) -> Self {
        Self { update, session }
    }
}

#[cfg(test)]
mod tests {
    use coblet_runtime::Secret;

    use super::LoginOptions;

    #[test]
    fn method_inputs_keep_secrets_out_of_debug_output() {
        let options = LoginOptions::new()
            .with_hint("steve@example.com")
            .with_param("server", "https://skin.example/api/yggdrasil")
            .with_secret("password", Secret::new("hunter2"));
        assert_eq!(
            options.params["server"],
            "https://skin.example/api/yggdrasil"
        );
        assert_eq!(options.secrets.get("password").unwrap().expose(), "hunter2");
        let shown = format!("{options:?}");
        assert!(
            shown.contains("skin.example") && !shown.contains("hunter2"),
            "{shown}"
        );
    }
}
