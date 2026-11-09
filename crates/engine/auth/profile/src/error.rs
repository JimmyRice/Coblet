use coblet_runtime::{StorageError, TransportError};
use coblet_types::ExtensionId;

/// Failure of a skin, cape or player lookup call.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    /// The access token was rejected; refresh the account and try again.
    #[error("the access token was rejected")]
    Unauthorized,
    /// The account is offline and cannot change its skin on Minecraft services.
    #[error("offline accounts have no online skins or capes")]
    Offline,
    /// The account's appearance service cannot make this change, or the
    /// account has no appearance service.
    #[error("appearance service `{service}` cannot {operation}")]
    Unsupported {
        /// ID of the service.
        service: ExtensionId,
        /// What was asked, such as `reset skins`.
        operation: &'static str,
    },
    /// The skin image is not a 64x64 or 64x32 PNG.
    #[error("invalid skin image: {0}")]
    InvalidSkin(String),
    /// The player wears the default skin, so there is no skin to change.
    #[error("the player wears the default skin")]
    NoSkin,
    /// The account does not own this cape.
    #[error("the account does not own cape `{0}`")]
    CapeNotOwned(String),
    /// No player has this name or UUID.
    #[error("no player named `{0}`")]
    UnknownPlayer(String),
    /// No skin in the library has this ID.
    #[error("no skin `{0}` in the library")]
    UnknownSkin(String),
    /// The HTTP exchange failed.
    #[error("{endpoint} request failed: {source}")]
    Transport {
        /// Short name of the endpoint.
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
    /// Reading or writing the skin library failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The skin library index is damaged or newer than this version.
    #[error("skin library index is not usable: {0}")]
    Library(String),
}
