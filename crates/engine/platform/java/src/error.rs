use std::path::PathBuf;

use coblet_catalog::CatalogError;
use coblet_download::DownloadError;
use coblet_runtime::StorageError;
use coblet_types::ExtensionId;

/// Failure to find, probe or install Java.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum JavaError {
    /// A `java` executable could not be run or printed no version.
    #[error("cannot probe {}: {reason}", .executable.display())]
    Probe {
        /// The executable.
        executable: PathBuf,
        /// Why.
        reason: String,
    },
    /// Fetching the runtime index or manifest failed.
    #[error(transparent)]
    Metadata(#[from] CatalogError),
    /// Downloading a runtime file failed.
    #[error("cannot download {path}: {source}")]
    Download {
        /// The file inside the runtime.
        path: String,
        /// The failure.
        #[source]
        source: DownloadError,
    },
    /// Reading or writing files failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The runtime manifest is unusable.
    #[error("invalid runtime manifest: {0}")]
    InvalidManifest(String),
    /// A Java provider failed or returned unusable metadata.
    #[error("java provider `{provider}`: {reason}")]
    Provider {
        /// The provider.
        provider: ExtensionId,
        /// What went wrong.
        reason: String,
    },
    /// A data Java provider definition is malformed.
    #[error("invalid java provider definition: {0}")]
    Definition(String),
    /// No installation meets the requirement.
    #[error("no java {major} for {arch} found and none can be installed: {reason}")]
    NotFound {
        /// Major version needed.
        major: u32,
        /// Architecture needed.
        arch: String,
        /// Why nothing could be installed.
        reason: String,
    },
}

impl JavaError {
    /// Creates a [`JavaError::Provider`].
    pub fn provider(provider: &ExtensionId, reason: impl Into<String>) -> Self {
        Self::Provider {
            provider: provider.clone(),
            reason: reason.into(),
        }
    }
}
