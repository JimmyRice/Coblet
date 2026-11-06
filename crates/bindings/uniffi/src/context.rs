use std::sync::Arc;

use coblet::catalog::Filter;
use coblet::launch::LaunchRequest;
use coblet::launch_command::LaunchAuth;
use coblet::mojang::GameVersion;
use coblet::types::{ExtensionId, Platform, PlatformNameError, PlatformTrait, PlayerName};

use crate::{CobletError, ErrorKind, GameHandle};

/// Game version from the catalog.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VersionInfo {
    /// Version ID, such as `1.20.1`.
    pub id: String,
    /// Manifest type, such as `release` or `old_beta`.
    pub version_type: String,
    /// Release time in RFC 3339.
    pub released: String,
    /// Whether the manifest names it the latest release.
    pub latest_release: bool,
}

impl From<&GameVersion> for VersionInfo {
    fn from(version: &GameVersion) -> Self {
        Self {
            id: version.id.as_str().to_owned(),
            version_type: version.version_type.as_str().to_owned(),
            released: version.released.to_string(),
            latest_release: version.latest_release,
        }
    }
}

/// How the game identifies the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LaunchMode {
    /// Local name and offline UUID.
    Offline,
    /// Demo mode.
    Demo,
}

/// Per-launch choices for [`Coblet::launch_with`].
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LaunchOptions {
    /// Player name.
    pub player_name: String,
    /// Offline or Demo.
    pub mode: LaunchMode,
    /// Working directory of the game; `<data_root>/games/<version>` when absent.
    #[uniffi(default = None)]
    pub game_dir: Option<String>,
    /// IDs of registered customizers to run, in order.
    #[uniffi(default = [])]
    pub customizers: Vec<String>,
    /// Launch ID for events, from `generate_operation_id`; random when
    /// absent.
    #[uniffi(default = None)]
    pub operation_id: Option<String>,
}

/// Platform as Platform Profiles see it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PlatformInfo {
    /// Operating system, such as `macos` or `freebsd`.
    pub os: String,
    /// Architecture, such as `aarch64` or `loongarch64`.
    pub arch: String,
    /// Operating system version, when known.
    #[uniffi(default = None)]
    pub os_version: Option<String>,
    /// C library, such as `glibc` or `musl`, when known.
    #[uniffi(default = None)]
    pub libc: Option<String>,
    /// Platform traits that profiles can match with `platform_traits`.
    #[uniffi(default = [])]
    pub traits: Vec<String>,
}

impl PlatformInfo {
    pub(crate) fn from_platform(platform: &Platform) -> Self {
        Self {
            os: platform.os().as_str().to_owned(),
            arch: platform.arch().as_str().to_owned(),
            os_version: platform.os_version().map(str::to_owned),
            libc: platform.libc().map(|libc| libc.as_str().to_owned()),
            traits: platform
                .traits()
                .iter()
                .map(|value| value.as_str().to_owned())
                .collect(),
        }
    }

    pub(crate) fn into_platform(self) -> Result<Platform, CobletError> {
        let invalid = |error: PlatformNameError| {
            CobletError::new(ErrorKind::InvalidArgument, error.to_string())
        };
        let mut platform = Platform::new(
            self.os.parse().map_err(invalid)?,
            self.arch.parse().map_err(invalid)?,
        );
        if let Some(version) = self.os_version {
            platform = platform.with_os_version(version);
        }
        if let Some(libc) = &self.libc {
            platform = platform.with_libc(libc.parse().map_err(invalid)?);
        }
        for name in &self.traits {
            platform = platform.with_trait(PlatformTrait::new(name).map_err(invalid)?);
        }
        Ok(platform)
    }
}

/// coblet library handle: version catalog and launching.
#[derive(Debug, uniffi::Object)]
pub struct Coblet {
    pub(crate) inner: coblet::Coblet,
}

impl Coblet {
    pub(crate) fn from_inner(inner: coblet::Coblet) -> Self {
        Self { inner }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Coblet {
    /// Creates a `Coblet` with the default transport and nothing added.
    ///
    /// Use [`CobletBuilder`](crate::CobletBuilder) to add Platform Profiles
    /// or launch customizers.
    ///
    /// # Errors
    ///
    /// Fails when the User-Agent is malformed or the transport cannot start.
    #[uniffi::constructor]
    pub fn new(data_root: String, user_agent: String) -> Result<Arc<Self>, CobletError> {
        crate::CobletBuilder::new(data_root, user_agent)?.build()
    }

    /// Returns the platform launches are adapted to, as Platform Profiles
    /// match it: detected from the build target, or set with
    /// `CobletBuilder.set_platform`.
    #[must_use]
    pub fn platform(&self) -> PlatformInfo {
        PlatformInfo::from_platform(self.inner.platform())
    }

    /// Lists game versions, newest first.
    ///
    /// # Errors
    ///
    /// Fails when the manifest is unavailable and not cached.
    pub async fn versions(&self) -> Result<Vec<VersionInfo>, CobletError> {
        let versions = self.inner.versions(&Filter::new()).await?;
        Ok(versions.iter().map(VersionInfo::from).collect())
    }

    /// Returns the latest release.
    ///
    /// # Errors
    ///
    /// Fails when the manifest is unavailable or names no latest release.
    pub async fn latest_release(&self) -> Result<VersionInfo, CobletError> {
        Ok(VersionInfo::from(&self.inner.latest_release().await?))
    }

    /// Downloads what a version needs and starts it.
    ///
    /// Without `game_dir` the game runs in `<data_root>/games/<version>`.
    ///
    /// # Errors
    ///
    /// Fails for unknown versions, unsupported platforms and failed launch steps.
    pub async fn launch(
        &self,
        version_id: String,
        player_name: String,
        mode: LaunchMode,
        game_dir: Option<String>,
    ) -> Result<Arc<GameHandle>, CobletError> {
        let options = LaunchOptions {
            player_name,
            mode,
            game_dir,
            customizers: Vec::new(),
            operation_id: None,
        };
        self.launch_with(version_id, options).await
    }

    /// Downloads what a version needs and starts it with these options.
    ///
    /// # Errors
    ///
    /// Fails for unknown versions or customizers, unsupported platforms and
    /// failed launch steps.
    pub async fn launch_with(
        &self,
        version_id: String,
        options: LaunchOptions,
    ) -> Result<Arc<GameHandle>, CobletError> {
        let invalid = |error: &dyn std::fmt::Display| {
            CobletError::new(ErrorKind::InvalidArgument, error.to_string())
        };
        let name = PlayerName::new(options.player_name).map_err(|error| invalid(&error))?;
        let auth = match options.mode {
            LaunchMode::Offline => LaunchAuth::offline(name),
            LaunchMode::Demo => LaunchAuth::demo(name),
        };
        let mut request = LaunchRequest::new(auth);
        if let Some(id) = options.operation_id {
            request = request.with_operation(crate::events::operation_id(&id)?);
        }
        for id in options.customizers {
            request =
                request.with_customizer(ExtensionId::new(id).map_err(|error| invalid(&error))?);
        }
        let game_dir = options.game_dir.map_or_else(
            || self.inner.data_root().join("games").join(&version_id),
            Into::into,
        );
        let session = self.inner.launch(&version_id, game_dir, request).await?;
        Ok(Arc::new(GameHandle::new(session)))
    }
}
