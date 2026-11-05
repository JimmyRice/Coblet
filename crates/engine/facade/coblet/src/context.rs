use std::path::{Path, PathBuf};
use std::sync::Arc;

use coblet_catalog::MetaCache;
use coblet_download::Downloader;
use coblet_runtime::{EventBus, EventSubscription, ProcessSpawner, Storage, Transport};
use coblet_types::Platform;

/// Entry point of coblet: shared services, the version catalog and launching.
///
/// Cloning is cheap; clones share everything. Every subsystem can also be
/// built without a `Coblet` from the trait objects it needs.
///
/// # Examples
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use coblet::{Coblet, runtime::UserAgent};
///
/// let coblet = Coblet::builder()
///     .data_root("/path/to/launcher-data")
///     .user_agent(UserAgent::new("MyLauncher/1.0 (contact@example.com)")?)
///     .build()?;
/// let latest = coblet.latest_release().await?;
/// println!("latest release: {}", latest.id);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Coblet {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) data_root: PathBuf,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) spawner: Arc<dyn ProcessSpawner>,
    pub(crate) platform: Platform,
    pub(crate) events: EventBus,
    pub(crate) meta: MetaCache,
    pub(crate) downloader: Downloader,
    #[cfg(feature = "mojang")]
    pub(crate) versions: crate::versions::VersionCatalog,
    #[cfg(feature = "launch")]
    pub(crate) launcher: coblet_launch::Launcher,
    #[cfg(feature = "launch")]
    pub(crate) profiles: coblet_platform::ProfileEngine,
}

impl std::fmt::Debug for Coblet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coblet")
            .field("data_root", &self.inner.data_root)
            .field("platform", &self.inner.platform)
            .finish_non_exhaustive()
    }
}

impl Coblet {
    pub(crate) fn from_inner(inner: Inner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Returns the data root.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.inner.data_root
    }

    /// Returns the platform launches are prepared for.
    #[must_use]
    pub fn platform(&self) -> &Platform {
        &self.inner.platform
    }

    /// Returns the transport.
    #[must_use]
    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.inner.transport
    }

    /// Returns the file access.
    #[must_use]
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.inner.storage
    }

    /// Returns the process spawner.
    #[must_use]
    pub fn spawner(&self) -> &Arc<dyn ProcessSpawner> {
        &self.inner.spawner
    }

    /// Returns the metadata cache.
    #[must_use]
    pub fn meta(&self) -> &MetaCache {
        &self.inner.meta
    }

    /// Returns the downloader.
    #[must_use]
    pub fn downloader(&self) -> &Downloader {
        &self.inner.downloader
    }

    /// Returns the event bus.
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.inner.events
    }

    /// Subscribes to progress, warnings and game log lines.
    #[must_use]
    pub fn subscribe(&self) -> EventSubscription {
        self.inner.events.subscribe()
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
