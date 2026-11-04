use std::path::PathBuf;

use coblet_catalog::CatalogError;
use coblet_download::DownloadError;
use coblet_java::JavaError;
use coblet_launch_command::CommandError;
use coblet_runtime::{ProcessError, ProcessExit, StorageError};

/// Failure to prepare or start the game.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// No suitable Java runtime.
    #[error(transparent)]
    Java(#[from] JavaError),
    /// The configured Java does not match what the version needs.
    #[error("{} is java {found_major} for {found_arch}, the version needs java {major} for {arch}", .executable.display())]
    JavaMismatch {
        /// The configured executable.
        executable: PathBuf,
        /// Major version found.
        found_major: u32,
        /// Architecture found.
        found_arch: String,
        /// Major version needed.
        major: u32,
        /// Architecture needed.
        arch: String,
    },
    /// Fetching metadata such as the asset index or a `.sha1` file failed.
    #[error(transparent)]
    Metadata(#[from] CatalogError),
    /// Some game files could not be downloaded.
    #[error("{count} file(s) could not be downloaded, first {}: {first}", .path.display())]
    Download {
        /// How many files failed.
        count: usize,
        /// The first file that failed.
        path: PathBuf,
        /// Its failure.
        #[source]
        first: DownloadError,
    },
    /// A library has no download address and is not on disk.
    #[error("library {} is missing and has no download address", .0.display())]
    MissingLibrary(PathBuf),
    /// A metadata value is unusable.
    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),
    /// A native jar could not be extracted.
    #[error("cannot extract natives from {}: {message}", .jar.display())]
    Natives {
        /// The jar.
        jar: PathBuf,
        /// Why.
        message: String,
    },
    /// Reading or writing files failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The command could not be built.
    #[error(transparent)]
    Command(#[from] CommandError),
    /// Starting a process failed.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// The pre-launch command failed.
    #[error("pre-launch command ended with {0}")]
    PreLaunch(ProcessExit),
    /// A step needs a value an earlier step did not provide.
    #[error("step `{step}` needs {needs}")]
    MissingState {
        /// The step.
        step: &'static str,
        /// What is missing.
        needs: &'static str,
    },
    /// A custom step failed.
    #[error("step `{step}` failed: {message}")]
    Step {
        /// The step.
        step: String,
        /// Why.
        message: String,
    },
    /// No launch step has this name.
    #[error("no launch step named `{0}`")]
    UnknownStep(String),
    /// The launch was cancelled.
    #[error("launch was cancelled")]
    Cancelled,
}
