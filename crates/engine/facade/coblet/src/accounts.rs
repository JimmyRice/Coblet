use coblet_auth::{
    Account, AccountId, AccountStore, AuthError, AuthFlow, LaunchSession, LoginOptions, RefreshMode,
};
use coblet_types::ExtensionId;

use crate::{Coblet, Error};

/// ID of the Microsoft auth method.
const MICROSOFT: &str = "coblet:microsoft";

impl Coblet {
    /// Returns the account store: the accounts in `data_root/accounts.json`,
    /// the default account and the account chosen for each instance.
    ///
    /// Reading it needs no network. The player profile of each account is
    /// the snapshot from its last login, token renewal or skin change; call
    /// [`refresh_profile`](Self::refresh_profile) for the current one.
    #[must_use]
    pub fn accounts(&self) -> &AccountStore {
        &self.inner.accounts
    }

    /// Starts a login with `method`, such as `coblet:offline` or
    /// `coblet:microsoft`. Show the flow's prompt, then pass the flow to
    /// [`finish_login`](Self::finish_login).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for Microsoft login without a Client
    /// ID, and [`Error::Auth`] when the method fails to start.
    pub async fn begin_login(
        &self,
        method: &ExtensionId,
        options: &LoginOptions,
    ) -> Result<AuthFlow, Error> {
        self.inner
            .accounts
            .begin_login(method, options)
            .await
            .map_err(auth_error)
    }

    /// Waits until the user has signed in and saves the account.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] with the reason the login failed.
    pub async fn finish_login(
        &self,
        method: &ExtensionId,
        flow: AuthFlow,
    ) -> Result<Account, Error> {
        self.inner
            .accounts
            .finish_login(method, flow)
            .await
            .map_err(auth_error)
    }

    /// Starts a Microsoft login.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn demo(coblet: coblet::Coblet) -> Result<(), coblet::Error> {
    /// use coblet::auth::LoginOptions;
    /// use coblet::runtime::AuthPrompt;
    ///
    /// let flow = coblet.begin_microsoft_login(&LoginOptions::new()).await?;
    /// if let AuthPrompt::DeviceCode { user_code, verification_uri, .. } = flow.prompt() {
    ///     println!("open {verification_uri} and enter {user_code}");
    /// }
    /// let account = coblet.finish_microsoft_login(flow).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] without a Client ID.
    #[cfg(feature = "auth-msa")]
    pub async fn begin_microsoft_login(&self, options: &LoginOptions) -> Result<AuthFlow, Error> {
        self.begin_login(&microsoft(), options).await
    }

    /// Waits for a Microsoft login and saves the account.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] with the reason the login failed.
    #[cfg(feature = "auth-msa")]
    pub async fn finish_microsoft_login(&self, flow: AuthFlow) -> Result<Account, Error> {
        self.finish_login(&microsoft(), flow).await
    }

    /// Returns a session to launch the account with, renewing its tokens
    /// first when they are about to expire.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] with [`AuthError::NoLicense`] when the
    /// account does not own the game (it can still launch in demo mode),
    /// [`Error::Unsupported`] for a Microsoft account without a Client ID,
    /// and the refresh error otherwise.
    pub async fn session(&self, account: &AccountId) -> Result<LaunchSession, Error> {
        self.inner
            .accounts
            .session(account, RefreshMode::IfNeeded)
            .await
            .map_err(auth_error)
    }

    /// Builds a launch request for the account, with its identity and any
    /// JVM arguments its auth method needs.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`session`](Self::session).
    #[cfg(feature = "launch")]
    pub async fn launch_request(
        &self,
        account: &AccountId,
    ) -> Result<coblet_launch::LaunchRequest, Error> {
        let session = self.session(account).await?;
        let mut request = coblet_launch::LaunchRequest::new(session.auth);
        request.jvm_args.extend(session.jvm_args);
        Ok(request)
    }
}

#[cfg(feature = "auth-msa")]
fn microsoft() -> ExtensionId {
    ExtensionId::new(MICROSOFT).unwrap_or_else(|_| unreachable!())
}

/// Reports a missing Microsoft method as unsupported rather than unknown.
pub(crate) fn auth_error(error: AuthError) -> Error {
    match error {
        AuthError::UnknownMethod(id) if id.as_str() == MICROSOFT => Error::Unsupported(
            if cfg!(feature = "auth-msa") {
                "microsoft login needs a client id and none was configured"
            } else {
                "microsoft login needs the auth-msa feature and a client id"
            }
            .to_owned(),
        ),
        AuthError::Unsupported(message) => Error::Unsupported(message),
        other => Error::Auth(other),
    }
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
