use coblet_catalog::CatalogError;
use coblet_runtime::{ProbeError, RegistryError, TransportError};

/// Failure of a [`Coblet`](crate::Coblet) call.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No transport was given and the build has no default one, or no
    /// User-Agent was given for the default one.
    #[error("{0}")]
    Config(&'static str),
    /// The default transport could not be created.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// Two extensions were given with the same ID.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The current platform could not be detected.
    #[error(transparent)]
    Platform(#[from] ProbeError),
    /// Fetching or reading metadata failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// No version has this ID.
    #[error("unknown game version `{0}`")]
    UnknownVersion(String),
    /// The version cannot run on this platform.
    #[cfg(feature = "launch")]
    #[error(transparent)]
    Profile(#[from] coblet_platform::ProfileError),
    /// The launch failed.
    #[cfg(feature = "launch")]
    #[error(transparent)]
    Launch(#[from] coblet_launch::LaunchError),
    /// A Mod Loader call failed.
    #[cfg(feature = "loaders")]
    #[error(transparent)]
    Loader(#[from] coblet_loader::LoaderError),
    /// Finding, describing or installing Java failed, or a Java provider
    /// definition is malformed.
    #[cfg(feature = "java")]
    #[error(transparent)]
    Java(#[from] coblet_java::JavaError),
    /// A version trait table is malformed.
    #[cfg(feature = "mojang")]
    #[error(transparent)]
    Traits(#[from] coblet_mojang::TraitError),
    /// The feature is not available in this setup, such as Microsoft login
    /// without a Client ID.
    #[error("{0}")]
    Unsupported(String),
    /// A login, refresh or account change failed.
    #[error(transparent)]
    Auth(#[from] coblet_auth::AuthError),
    /// A skin or cape change failed.
    #[cfg(feature = "appearance")]
    #[error(transparent)]
    Player(#[from] coblet_player::PlayerError),
}
