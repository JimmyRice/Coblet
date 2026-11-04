use std::path::PathBuf;

use coblet_catalog::CatalogError;
use coblet_download::DownloadError;
use coblet_runtime::StorageError;

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
    /// Mojang has no runtime for this platform and component.
    #[error("mojang has no `{component}` runtime for `{platform}`")]
    NotAvailable {
        /// Runtime platform key, such as `mac-os-arm64`.
        platform: String,
        /// Runtime component, such as `jre-legacy`.
        component: String,
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
