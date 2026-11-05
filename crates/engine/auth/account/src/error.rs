use coblet_runtime::{SecretKeyError, SecretStoreError, StorageError, TransportError};
use coblet_types::{ExtensionId, PlayerNameError};

use crate::{AccountId, AccountState};

/// Failure of a login, a refresh or an account store call.
///
/// Every step of the Microsoft login maps its known failures to a variant
/// of its own, so a UI can explain what the player has to do.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The auth method is not available, for example Microsoft login without a Client ID.
    #[error("{0}")]
    Unsupported(String),
    /// No auth method has this ID.
    #[error("unknown auth method `{0}`")]
    UnknownMethod(ExtensionId),
    /// No account has this ID.
    #[error("unknown account `{0}`")]
    UnknownAccount(AccountId),
    /// The player name is not a valid Minecraft name.
    #[error(transparent)]
    InvalidName(#[from] PlayerNameError),
    /// The login options are incomplete or contradict each other.
    #[error("invalid login options: {0}")]
    InvalidOptions(String),
    /// The HTTP exchange failed.
    #[error("{endpoint} request failed: {source}")]
    Transport {
        /// Short name of the endpoint, such as `xsts`.
        endpoint: &'static str,
        /// The transport error.
        #[source]
        source: TransportError,
    },
    /// A service answered with an unexpected status.
    #[error("{endpoint} returned http {status}: {message}")]
    Http {
        /// Short name of the endpoint.
        endpoint: &'static str,
        /// HTTP status code.
        status: u16,
        /// Message from the service, shortened.
        message: String,
    },
    /// A service answered with a body that could not be understood.
    #[error("{endpoint} returned an invalid response: {message}")]
    InvalidResponse {
        /// Short name of the endpoint.
        endpoint: &'static str,
        /// What was wrong.
        message: String,
    },
    /// A service asked to slow down.
    #[error("{endpoint} is rate limited, try again later")]
    RateLimited {
        /// Short name of the endpoint.
        endpoint: &'static str,
    },
    /// The user declined the authorization request.
    #[error("the authorization request was declined")]
    Declined,
    /// The device code or the browser login expired before the user finished.
    #[error("the login expired before it was completed")]
    Expired,
    /// The login was cancelled.
    #[error("the login was cancelled")]
    Cancelled,
    /// The browser redirect carries a different `state` than the login sent.
    #[error("the login redirect does not belong to this login")]
    StateMismatch,
    /// The browser redirect could not be parsed or carries no code.
    #[error("invalid login redirect: {0}")]
    InvalidCallback(String),
    /// The login no longer waits for a redirect.
    #[error("the login no longer accepts a redirect")]
    CallbackClosed,
    /// The identity provider rejected the request with an OAuth error.
    #[error("oauth error {error}: {description}")]
    OAuth {
        /// OAuth error code, such as `invalid_client`.
        error: String,
        /// Description from the provider.
        description: String,
    },
    /// The stored refresh token was revoked or expired; the user has to sign in again.
    #[error("the saved login is no longer valid, sign in again")]
    InvalidGrant,
    /// The Microsoft account has no Xbox profile yet. Signing in once at
    /// xbox.com creates one.
    #[error("the microsoft account has no xbox profile, sign in at xbox.com once to create it")]
    NoXboxAccount,
    /// The account belongs to a child and must be added to a Microsoft family by an adult.
    #[error("the account belongs to a child and must be added to a family by an adult")]
    ChildAccount,
    /// Xbox Live requires adult verification for this account (South Korea).
    #[error("xbox live requires adult verification for this account")]
    AdultVerificationRequired,
    /// Xbox Live is not available in the country of the account.
    #[error("xbox live is not available in the country of this account")]
    RegionUnavailable,
    /// The account is banned from Xbox Live.
    #[error("the account is banned from xbox live")]
    XboxBanned,
    /// Xbox Live refused with another error code.
    #[error("xbox live refused the login with error {0}")]
    Xbox(u64),
    /// The Client ID has not been approved for Minecraft services.
    #[error("the client id is not approved for minecraft services, apply for access first")]
    ClientNotApproved,
    /// The account does not own Minecraft; it can still launch the demo.
    #[error("the account does not own minecraft")]
    NoLicense,
    /// The account owns Minecraft but has no player profile yet.
    #[error("the account owns minecraft but has not created a profile at minecraft.net yet")]
    NoProfile,
    /// The secret store failed.
    #[error(transparent)]
    Secrets(#[from] SecretStoreError),
    /// A secret name produced an invalid key.
    #[error(transparent)]
    SecretKey(#[from] SecretKeyError),
    /// Reading or writing account files failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The account file is damaged or written by a newer version.
    #[error("account file {path} is not usable: {message}")]
    AccountFile {
        /// The file.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// A third-party auth method failed.
    #[error("{0}")]
    Failed(String),
}

impl AuthError {
    /// Returns the account state this error implies, if it says anything
    /// about the account rather than the connection.
    #[must_use]
    pub fn implied_state(&self) -> Option<AccountState> {
        match self {
            Self::InvalidGrant
            | Self::NoXboxAccount
            | Self::ChildAccount
            | Self::AdultVerificationRequired
            | Self::XboxBanned => Some(AccountState::NeedsReauth),
            Self::NoLicense => Some(AccountState::NoLicense),
            Self::NoProfile => Some(AccountState::NoProfile),
            _ => None,
        }
    }

    /// Reports whether repeating the call later can succeed without the user.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { source, .. } => source.is_retryable(),
            Self::RateLimited { .. } => true,
            Self::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }
}
