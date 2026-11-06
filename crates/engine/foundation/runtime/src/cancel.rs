pub use tokio_util::sync::CancellationToken;

/// Error returned by work that stopped because its [`CancellationToken`] fired.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, thiserror::Error)]
#[error("operation was cancelled")]
pub struct Cancelled;

/// Returns [`Cancelled`] once `token` has fired.
///
/// Steps call this at IO boundaries (before each chunk, file or request) so
/// cancellation takes effect without waiting for the whole step.
///
/// # Errors
///
/// Returns [`Cancelled`] when the token is cancelled.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{CancellationToken, Cancelled, ensure_not_cancelled};
///
/// let token = CancellationToken::new();
/// assert_eq!(ensure_not_cancelled(&token), Ok(()));
/// token.cancel();
/// assert_eq!(ensure_not_cancelled(&token), Err(Cancelled));
/// ```
pub fn ensure_not_cancelled(token: &CancellationToken) -> Result<(), Cancelled> {
    if token.is_cancelled() {
        Err(Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/cancel.rs"]
mod tests;
