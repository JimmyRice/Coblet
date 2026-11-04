use std::path::PathBuf;

use coblet_runtime::{ArtifactKind, Expectation, Sources};

/// One file to fetch: where from, what it must match and where it goes.
///
/// # Examples
///
/// ```
/// use coblet_download::{Destination, DownloadTask};
/// use coblet_runtime::{ArtifactKind, Expectation, HttpUrl, Sources};
/// use coblet_types::FileHash;
///
/// let task = DownloadTask::new(
///     Sources::single(HttpUrl::new("https://libraries.minecraft.net/a/b/1/b-1.jar")?),
///     Expectation::hash(FileHash::new("sha1", &"0".repeat(40))?, Some(1024)),
///     Destination::Path("/data/libraries/a/b/1/b-1.jar".into()),
///     ArtifactKind::Library,
/// );
/// assert_eq!(task.expect.size(), Some(1024));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadTask {
    /// Addresses tried in order; later ones are fallbacks.
    pub sources: Sources,
    /// Size and hashes the file must match. Tasks without a hash say
    /// [`Expectation::Unverified`] explicitly.
    pub expect: Expectation,
    /// Where the file goes.
    pub dest: Destination,
    /// Order among tasks passed to [`Downloader::download_all`](crate::Downloader::download_all).
    pub priority: Priority,
    /// What the file is.
    pub kind: ArtifactKind,
}

impl DownloadTask {
    /// Creates a task with [`Priority::Normal`].
    #[must_use]
    pub fn new(
        sources: Sources,
        expect: Expectation,
        dest: Destination,
        kind: ArtifactKind,
    ) -> Self {
        Self {
            sources,
            expect,
            dest,
            priority: Priority::Normal,
            kind,
        }
    }

    /// Sets the priority.
    #[must_use]
    pub fn with_priority(self, priority: Priority) -> Self {
        Self { priority, ..self }
    }
}

/// Where a downloaded file is stored.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// A fixed path, such as `libraries/<maven path>`.
    Path(PathBuf),
    /// The content-addressed cache `cache/objects/<algorithm>/<hash>`, keyed by
    /// the primary hash, optionally linked to a path (hard link, then copy).
    ContentAddressed {
        /// Path that receives a link to the cached object.
        link: Option<PathBuf>,
    },
}

/// Scheduling order of a task; higher priorities start first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Background work, such as asset objects.
    Low,
    /// The default.
    #[default]
    Normal,
    /// Files needed before anything else, such as the client jar.
    High,
}

/// How a task ended successfully.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// The file was fetched; `bytes` arrived over the network.
    Downloaded {
        /// Bytes transferred in this run (less than the size after a resume).
        bytes: u64,
    },
    /// A matching file was already at the destination.
    AlreadyPresent,
    /// The file came from the content-addressed cache.
    FromCache,
}
