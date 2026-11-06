use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use coblet_runtime::{AuthPrompt, BoxFuture};
use tokio::sync::oneshot;

use crate::{AccountUpdate, AuthError};

/// Login in progress: the prompt to show and the work that finishes it.
///
/// Show [`prompt`](Self::prompt) to the user, then await
/// [`finish`](Self::finish). A browser login with an external redirect also
/// needs the redirect URI handed to [`callback`](Self::callback).
///
/// # Examples
///
/// ```
/// use coblet_auth::{AccountState, AccountUpdate, AuthFlow};
/// use coblet_runtime::AuthPrompt;
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let flow = AuthFlow::new(AuthPrompt::Nothing, async {
///     Ok(AccountUpdate::new(AccountState::Ready))
/// });
/// assert_eq!(flow.prompt(), &AuthPrompt::Nothing);
/// assert_eq!(flow.finish().await?.state, AccountState::Ready);
/// # Ok::<(), coblet_auth::AuthError>(())
/// # }).unwrap();
/// ```
pub struct AuthFlow {
    prompt: AuthPrompt,
    callback: Option<CallbackSender>,
    outcome: BoxFuture<'static, Result<AccountUpdate, AuthError>>,
}

impl AuthFlow {
    /// Creates a flow from its prompt and the future that completes it.
    pub fn new(
        prompt: AuthPrompt,
        outcome: impl Future<Output = Result<AccountUpdate, AuthError>> + Send + 'static,
    ) -> Self {
        Self {
            prompt,
            callback: None,
            outcome: Box::pin(outcome),
        }
    }

    /// Lets callers deliver a browser redirect through `sender`.
    #[must_use]
    pub fn with_callback(mut self, sender: CallbackSender) -> Self {
        self.callback = Some(sender);
        self
    }

    /// Returns what the user has to do.
    #[must_use]
    pub fn prompt(&self) -> &AuthPrompt {
        &self.prompt
    }

    /// Returns the sender for the browser redirect, if the flow waits for one.
    #[must_use]
    pub fn callback(&self) -> Option<&CallbackSender> {
        self.callback.as_ref()
    }

    /// Waits until the user has signed in and returns the account data.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthError`] of the step that failed.
    pub async fn finish(self) -> Result<AccountUpdate, AuthError> {
        self.outcome.await
    }
}

impl std::fmt::Debug for AuthFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthFlow")
            .field("prompt", &self.prompt)
            .field("callback", &self.callback.is_some())
            .finish_non_exhaustive()
    }
}

/// Hands a browser redirect URI to the login that waits for it.
///
/// Clones share the channel; the first URI submitted wins.
#[derive(Clone, Debug)]
pub struct CallbackSender(Arc<Mutex<Option<oneshot::Sender<String>>>>);

/// Receiving end of a [`CallbackSender`], held by the waiting login.
#[derive(Debug)]
pub struct CallbackReceiver(oneshot::Receiver<String>);

/// Creates a connected sender and receiver.
#[must_use]
pub fn callback_channel() -> (CallbackSender, CallbackReceiver) {
    let (tx, rx) = oneshot::channel();
    (
        CallbackSender(Arc::new(Mutex::new(Some(tx)))),
        CallbackReceiver(rx),
    )
}

impl CallbackSender {
    /// Delivers the full redirect URI, such as
    /// `myapp://auth?code=...&state=...`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CallbackClosed`] when a URI was already
    /// submitted or the login has ended.
    pub fn submit(&self, uri: impl Into<String>) -> Result<(), AuthError> {
        let sender = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or(AuthError::CallbackClosed)?;
        sender
            .send(uri.into())
            .map_err(|_| AuthError::CallbackClosed)
    }

    /// Reports whether the login still waits for a URI.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|sender| !sender.is_closed())
    }
}

impl CallbackReceiver {
    /// Waits for the redirect URI.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CallbackClosed`] when every sender was dropped.
    pub async fn recv(self) -> Result<String, AuthError> {
        self.0.await.map_err(|_| AuthError::CallbackClosed)
    }
}

#[cfg(test)]
#[path = "../tests/flow.rs"]
mod tests;
