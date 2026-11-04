use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use coblet_catalog::MetaCache;
use coblet_download::{Downloader, DownloaderConfig};
use coblet_runtime::{
    EventBus, PlatformProbe, ProcessSpawner, StdPlatformProbe, Storage, TokioSpawner, TokioStorage,
    Transport, UserAgent,
};
use coblet_types::Platform;

use crate::sdk::Inner;
use crate::{Sdk, SdkError};

/// Builder state: the data root is missing.
#[derive(Debug)]
pub struct NoRoot;
/// Builder state: the data root is set and [`SdkBuilder::build`] is available.
#[derive(Debug)]
pub struct WithRoot;

/// Events kept for slow subscribers before they miss some.
const EVENT_CAPACITY: usize = 1024;

/// Builder of an [`Sdk`]; [`build`](SdkBuilder::build) needs a data root.
#[must_use]
pub struct SdkBuilder<State> {
    data_root: Option<PathBuf>,
    user_agent: Option<UserAgent>,
    transport: Option<Arc<dyn Transport>>,
    storage: Option<Arc<dyn Storage>>,
    spawner: Option<Arc<dyn ProcessSpawner>>,
    platform: Option<Platform>,
    events: Option<EventBus>,
    #[cfg(feature = "launch")]
    launcher: Option<coblet_launch::Launcher>,
    state: PhantomData<State>,
}

impl<State> std::fmt::Debug for SdkBuilder<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkBuilder")
            .field("data_root", &self.data_root)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl Sdk {
    /// Starts a builder.
    pub fn builder() -> SdkBuilder<NoRoot> {
        SdkBuilder {
            data_root: None,
            user_agent: None,
            transport: None,
            storage: None,
            spawner: None,
            platform: None,
            events: None,
            #[cfg(feature = "launch")]
            launcher: None,
            state: PhantomData,
        }
    }
}

impl SdkBuilder<NoRoot> {
    /// Sets the directory for shared game files, caches and runtimes; a
    /// relative path is resolved against the current directory.
    pub fn data_root(self, path: impl Into<PathBuf>) -> SdkBuilder<WithRoot> {
        SdkBuilder {
            data_root: Some(path.into()),
            user_agent: self.user_agent,
            transport: self.transport,
            storage: self.storage,
            spawner: self.spawner,
            platform: self.platform,
            events: self.events,
            #[cfg(feature = "launch")]
            launcher: self.launcher,
            state: PhantomData,
        }
    }
}

impl<State> SdkBuilder<State> {
    /// Sets the User-Agent of the default transport, such as `MyLauncher/1.0 (contact@example.com)`.
    pub fn user_agent(mut self, user_agent: UserAgent) -> Self {
        self.user_agent = Some(user_agent);
        self
    }

    /// Uses this transport instead of the default one.
    pub fn transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Uses this file access instead of `tokio::fs`.
    pub fn storage(mut self, storage: Arc<dyn Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Uses this process spawner instead of `tokio::process`.
    pub fn spawner(mut self, spawner: Arc<dyn ProcessSpawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Describes the platform instead of detecting it.
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = Some(platform);
        self
    }

    /// Publishes events on this bus instead of a new one.
    pub fn events(mut self, events: EventBus) -> Self {
        self.events = Some(events);
        self
    }

    /// Launches with this step chain instead of the built-in one.
    #[cfg(feature = "launch")]
    pub fn launcher(mut self, launcher: coblet_launch::Launcher) -> Self {
        self.launcher = Some(launcher);
        self
    }
}

impl SdkBuilder<WithRoot> {
    fn transport_or_default(&mut self) -> Result<Arc<dyn Transport>, SdkError> {
        if let Some(transport) = self.transport.take() {
            return Ok(transport);
        }
        #[cfg(feature = "tls-rustls")]
        {
            let user_agent = self.user_agent.as_ref().ok_or(SdkError::Config(
                "the default transport needs a user agent; call user_agent()",
            ))?;
            Ok(Arc::new(coblet_runtime::ReqwestTransport::new(user_agent)?))
        }
        #[cfg(not(feature = "tls-rustls"))]
        Err(SdkError::Config(
            "this build has no default transport; call transport()",
        ))
    }

    /// Creates the SDK.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::Config`] without a transport or User-Agent,
    /// [`SdkError::Transport`] when the default transport cannot start and
    /// [`SdkError::Platform`] when the platform cannot be detected.
    pub fn build(mut self) -> Result<Sdk, SdkError> {
        let transport = self.transport_or_default()?;
        // The game and the Java runtime run with other working directories,
        // so every path derived from the data root must be absolute.
        let data_root = self.data_root.take().unwrap_or_default();
        let data_root = std::path::absolute(&data_root).unwrap_or(data_root);
        let storage = self
            .storage
            .take()
            .unwrap_or_else(|| Arc::new(TokioStorage::new()));
        let spawner = self
            .spawner
            .take()
            .unwrap_or_else(|| Arc::new(TokioSpawner::new()));
        let platform = match self.platform.take() {
            Some(platform) => platform,
            None => StdPlatformProbe.detect()?,
        };
        let events = self
            .events
            .take()
            .unwrap_or_else(|| EventBus::new(EVENT_CAPACITY));
        let meta = MetaCache::new(
            Arc::clone(&transport),
            Arc::clone(&storage),
            data_root.join("meta"),
        );
        let config = DownloaderConfig::default()
            .with_limits(
                NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN),
                NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
            )
            .with_cache_root(data_root.join("cache"));
        let downloader = Downloader::new(Arc::clone(&transport), Arc::clone(&storage), config);
        Ok(Sdk::from_inner(Inner {
            #[cfg(feature = "mojang")]
            versions: crate::versions::version_catalog(meta.clone(), events.clone()),
            #[cfg(feature = "launch")]
            launcher: self.launcher.take().unwrap_or_default(),
            data_root,
            transport,
            storage,
            spawner,
            platform,
            events,
            meta,
            downloader,
        }))
    }
}
