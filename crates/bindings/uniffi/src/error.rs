use coblet::auth::AuthError;
use coblet::platform::ProfileError;
use coblet::player::PlayerError;

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
    /// A login failed or the account has to sign in again.
    Auth,
    /// The account does not own the game; it can launch in demo mode.
    NoLicense,
    /// The account has no player profile yet.
    NoProfile,
    /// The operation was cancelled.
    Cancelled,
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

    /// Returns the category.
    pub(crate) fn kind(&self) -> ErrorKind {
        let Self::Failed { kind, .. } = self;
        *kind
    }
}

impl From<uniffi::UnexpectedUniFFICallbackError> for CobletError {
    fn from(error: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::new(
            ErrorKind::Other,
            format!("callback failed: {}", error.reason),
        )
    }
}

impl From<coblet::Error> for CobletError {
    fn from(error: coblet::Error) -> Self {
        let kind = match &error {
            coblet::Error::Config(_)
            | coblet::Error::Registry(_)
            | coblet::Error::Profile(ProfileError::InvalidProfile { .. }) => {
                ErrorKind::InvalidArgument
            }
            coblet::Error::Transport(_) | coblet::Error::Catalog(_) => ErrorKind::Network,
            coblet::Error::UnknownVersion(_) => ErrorKind::NotFound,
            coblet::Error::Profile(ProfileError::Failed { .. }) => ErrorKind::Other,
            coblet::Error::Profile(_)
            | coblet::Error::Unsupported(_)
            | coblet::Error::Launch(coblet::launch::LaunchError::Unsupported(_)) => {
                ErrorKind::Unsupported
            }
            coblet::Error::Launch(_) => ErrorKind::Launch,
            coblet::Error::Loader(error) => loader_kind(error),
            coblet::Error::Java(error) => java_kind(error),
            coblet::Error::Auth(error) => auth_kind(error),
            coblet::Error::Player(error) => player_kind(error),
            coblet::Error::Traits(error) => traits_kind(error),
            _ => ErrorKind::Other,
        };
        Self::new(kind, error.to_string())
    }
}

fn loader_kind(error: &coblet::loader::LoaderError) -> ErrorKind {
    use coblet::loader::LoaderError;
    match error {
        LoaderError::UnknownLoader(_) | LoaderError::UnknownVersion { .. } => ErrorKind::NotFound,
        LoaderError::InvalidDefinition { .. } => ErrorKind::InvalidArgument,
        LoaderError::Catalog(_) => ErrorKind::Network,
        _ => ErrorKind::Other,
    }
}

fn traits_kind(error: &coblet::mojang::TraitError) -> ErrorKind {
    use coblet::mojang::TraitError;
    match error {
        TraitError::Table(_) => ErrorKind::InvalidArgument,
        TraitError::Metadata(_) => ErrorKind::Network,
        _ => ErrorKind::Other,
    }
}

fn java_kind(error: &coblet::java::JavaError) -> ErrorKind {
    use coblet::java::JavaError;
    match error {
        JavaError::Definition(_) => ErrorKind::InvalidArgument,
        JavaError::Metadata(_) | JavaError::Download { .. } => ErrorKind::Network,
        JavaError::NotFound { .. } => ErrorKind::NotFound,
        _ => ErrorKind::Other,
    }
}

fn auth_kind(error: &AuthError) -> ErrorKind {
    match error {
        AuthError::Unsupported(_) => ErrorKind::Unsupported,
        AuthError::UnknownMethod(_) | AuthError::UnknownAccount(_) => ErrorKind::NotFound,
        AuthError::InvalidName(_)
        | AuthError::InvalidOptions(_)
        | AuthError::InvalidCallback(_)
        | AuthError::SecretKey(_) => ErrorKind::InvalidArgument,
        AuthError::Transport { .. }
        | AuthError::Http { .. }
        | AuthError::InvalidResponse { .. }
        | AuthError::RateLimited { .. } => ErrorKind::Network,
        AuthError::Cancelled => ErrorKind::Cancelled,
        AuthError::CallbackClosed => ErrorKind::InvalidState,
        AuthError::NoLicense => ErrorKind::NoLicense,
        AuthError::NoProfile => ErrorKind::NoProfile,
        AuthError::Declined
        | AuthError::Expired
        | AuthError::StateMismatch
        | AuthError::OAuth { .. }
        | AuthError::InvalidGrant
        | AuthError::NoXboxAccount
        | AuthError::ChildAccount
        | AuthError::AdultVerificationRequired
        | AuthError::RegionUnavailable
        | AuthError::XboxBanned
        | AuthError::Xbox(_)
        | AuthError::ClientNotApproved => ErrorKind::Auth,
        _ => ErrorKind::Other,
    }
}

fn player_kind(error: &PlayerError) -> ErrorKind {
    match error {
        PlayerError::Unauthorized => ErrorKind::Auth,
        PlayerError::Offline | PlayerError::Unsupported { .. } | PlayerError::NoService(_) => {
            ErrorKind::Unsupported
        }
        PlayerError::Definition(_) | PlayerError::InvalidSkin(_) | PlayerError::CapeNotOwned(_) => {
            ErrorKind::InvalidArgument
        }
        PlayerError::NoSkin => ErrorKind::InvalidState,
        PlayerError::UnknownPlayer(_) | PlayerError::UnknownSkin(_) => ErrorKind::NotFound,
        PlayerError::Transport { .. }
        | PlayerError::Http { .. }
        | PlayerError::InvalidResponse { .. } => ErrorKind::Network,
        _ => ErrorKind::Other,
    }
}
