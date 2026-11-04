use coblet::SdkError;

/// Category of a [`CobletError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ErrorKind {
    /// Anything else.
    Other,
    /// An argument was malformed.
    InvalidArgument,
    /// Metadata or files could not be fetched.
    Network,
    /// A version does not exist.
    NotFound,
    /// The version cannot run on this platform.
    Unsupported,
    /// A launch step failed.
    Launch,
    /// A call came in the wrong order, such as waiting twice for a game.
    InvalidState,
}

/// Error of every binding call: a kind and a message.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CobletError {
    /// The call failed.
    #[error("{message}")]
    Failed {
        /// The category.
        kind: ErrorKind,
        /// Human-readable message.
        message: String,
    },
}

impl CobletError {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Failed {
            kind,
            message: message.into(),
        }
    }
}

impl From<SdkError> for CobletError {
    fn from(error: SdkError) -> Self {
        let kind = match &error {
            SdkError::Config(_) => ErrorKind::InvalidArgument,
            SdkError::Transport(_) | SdkError::Catalog(_) => ErrorKind::Network,
            SdkError::UnknownVersion(_) => ErrorKind::NotFound,
            SdkError::Profile(_) => ErrorKind::Unsupported,
            SdkError::Launch(_) => ErrorKind::Launch,
            _ => ErrorKind::Other,
        };
        Self::new(kind, error.to_string())
    }
}
