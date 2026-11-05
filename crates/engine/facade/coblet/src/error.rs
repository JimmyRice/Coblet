use coblet_catalog::CatalogError;
use coblet_runtime::{ProbeError, RegistryError, TransportError};

/// Failure of an [`Sdk`](crate::Sdk) call.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
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
}
