use std::sync::{Mutex, PoisonError};

use coblet::auth::{AuthFlow, CallbackSender};
use coblet::runtime::CancellationToken;
use coblet::types::ExtensionId;

use crate::{AccountInfo, CobletError, ErrorKind, LoginPrompt};

/// Login in progress, from `Coblet.begin_microsoft_login`.
///
/// Show `prompt()` to the user, then await `finish()`. A flow finishes once;
/// `cancel()` stops it from any thread.
#[derive(uniffi::Object)]
pub struct LoginFlow {
    coblet: coblet::Coblet,
    method: ExtensionId,
    prompt: LoginPrompt,
    callback: Option<CallbackSender>,
    flow: Mutex<Option<AuthFlow>>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for LoginFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginFlow")
            .field("method", &self.method)
            .field("prompt", &self.prompt)
            .finish_non_exhaustive()
    }
}

impl LoginFlow {
    pub(crate) fn new(coblet: coblet::Coblet, method: ExtensionId, flow: AuthFlow) -> Self {
        Self {
            coblet,
            method,
            prompt: LoginPrompt::from(flow.prompt()),
            callback: flow.callback().cloned(),
            flow: Mutex::new(Some(flow)),
            cancel: CancellationToken::new(),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl LoginFlow {
    /// Returns what the user has to do.
    #[must_use]
    pub fn prompt(&self) -> LoginPrompt {
        self.prompt.clone()
    }

    /// Returns whether the login waits for a redirect URI from
    /// `submit_redirect`, as logins with `LoginInteraction.BrowserRedirect`
    /// do until the first one arrives.
    #[must_use]
    pub fn accepts_redirect(&self) -> bool {
        self.callback.as_ref().is_some_and(CallbackSender::is_open)
    }

    /// Hands the full URI the browser was redirected to, with its `code`
    /// and `state` parameters, to the waiting login.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidState` when the login does not wait for a
    /// redirect or already has one.
    pub fn submit_redirect(&self, uri: String) -> Result<(), CobletError> {
        let callback = self.callback.as_ref().ok_or_else(|| {
            CobletError::new(
                ErrorKind::InvalidState,
                "this login does not wait for a redirect",
            )
        })?;
        callback
            .submit(uri)
            .map_err(|error| CobletError::from(coblet::Error::Auth(error)))
    }

    /// Stops the login; a pending or later `finish` fails with `Cancelled`.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Waits until the user has signed in, saves the account and returns it.
    ///
    /// # Errors
    ///
    /// Fails with `Cancelled` after `cancel`, `InvalidState` when called
    /// twice, `Auth` when the login was declined, expired or refused by
    /// Xbox Live, and `Network` when a service cannot be reached. Accounts
    /// without the game or a profile are saved with the state
    /// `NoLicense` or `NoProfile` rather than failing.
    pub async fn finish(&self) -> Result<AccountInfo, CobletError> {
        let flow = self
            .flow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                CobletError::new(ErrorKind::InvalidState, "the login was already finished")
            })?;
        let finish = self.coblet.finish_login(&self.method, flow);
        match self.cancel.run_until_cancelled(finish).await {
            Some(account) => Ok(AccountInfo::from(&account?)),
            None => Err(CobletError::new(
                ErrorKind::Cancelled,
                "the login was cancelled",
            )),
        }
    }
}
