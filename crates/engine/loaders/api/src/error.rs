use coblet_catalog::CatalogError;
use coblet_types::{ExtensionId, GameVersionId};

/// Failure of a Mod Loader call.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    /// Fetching or reading metadata failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// No Mod Loader is registered under this ID.
    #[error("no mod loader registered as `{0}`")]
    UnknownLoader(ExtensionId),
    /// The Mod Loader has no such version for the game version.
    #[error("{loader} has no version `{version}` for minecraft {game}")]
    UnknownVersion {
        /// The Mod Loader.
        loader: ExtensionId,
        /// The game version.
        game: GameVersionId,
        /// The requested Mod Loader version.
        version: String,
    },
    /// The Mod Loader metadata is malformed or inconsistent.
    #[error("invalid metadata from {loader}: {message}")]
    InvalidMetadata {
        /// The Mod Loader.
        loader: ExtensionId,
        /// What is wrong.
        message: String,
    },
    /// A data-form Mod Loader definition is malformed.
    #[error("invalid mod loader definition {name}: {message}")]
    InvalidDefinition {
        /// File name or loader ID.
        name: String,
        /// The parser's description.
        message: String,
    },
    /// The Mod Loader failed, such as one written in another language that
    /// raised an error.
    #[error("mod loader {loader} failed: {message}")]
    Failed {
        /// The Mod Loader.
        loader: ExtensionId,
        /// What went wrong.
        message: String,
    },
}

impl LoaderError {
    /// Creates a [`LoaderError::InvalidMetadata`].
    #[must_use]
    pub fn invalid(loader: &ExtensionId, message: impl Into<String>) -> Self {
        Self::InvalidMetadata {
            loader: loader.clone(),
            message: message.into(),
        }
    }

    /// Creates a [`LoaderError::Failed`].
    #[must_use]
    pub fn failed(loader: &ExtensionId, message: impl Into<String>) -> Self {
        Self::Failed {
            loader: loader.clone(),
            message: message.into(),
        }
    }
}

impl From<LoaderError> for CatalogError {
    fn from(error: LoaderError) -> Self {
        match error {
            LoaderError::Catalog(error) => error,
            other => Self::Failed(other.to_string()),
        }
    }
}
