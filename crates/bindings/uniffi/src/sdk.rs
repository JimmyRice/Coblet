use std::sync::Arc;

use coblet::Sdk;
use coblet::catalog::Filter;
use coblet::launch::LaunchRequest;
use coblet::launch_command::LaunchAuth;
use coblet::mojang::GameVersion;
use coblet::runtime::UserAgent;
use coblet::types::PlayerName;

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

/// coblet SDK: version catalog and launching.
#[derive(Debug, uniffi::Object)]
pub struct CobletSdk {
    sdk: Sdk,
}

#[uniffi::export(async_runtime = "tokio")]
impl CobletSdk {
    /// Creates an SDK with the default transport.
    ///
    /// # Errors
    ///
    /// Fails when the User-Agent is malformed or the transport cannot start.
    #[uniffi::constructor]
    pub fn new(data_root: String, user_agent: String) -> Result<Arc<Self>, CobletError> {
        let user_agent = UserAgent::new(user_agent)
            .map_err(|error| CobletError::new(ErrorKind::InvalidArgument, error.to_string()))?;
        let sdk = Sdk::builder()
            .data_root(data_root)
            .user_agent(user_agent)
            .build()?;
        Ok(Arc::new(Self { sdk }))
    }

    /// Lists game versions, newest first.
    ///
    /// # Errors
    ///
    /// Fails when the manifest is unavailable and not cached.
    pub async fn versions(&self) -> Result<Vec<VersionInfo>, CobletError> {
        let versions = self.sdk.versions(&Filter::new()).await?;
        Ok(versions.iter().map(VersionInfo::from).collect())
    }

    /// Returns the latest release.
    ///
    /// # Errors
    ///
    /// Fails when the manifest is unavailable or names no latest release.
    pub async fn latest_release(&self) -> Result<VersionInfo, CobletError> {
        Ok(VersionInfo::from(&self.sdk.latest_release().await?))
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
        let name = PlayerName::new(player_name)
            .map_err(|error| CobletError::new(ErrorKind::InvalidArgument, error.to_string()))?;
        let auth = match mode {
            LaunchMode::Offline => LaunchAuth::offline(name),
            LaunchMode::Demo => LaunchAuth::demo(name),
        };
        let game_dir = game_dir.map_or_else(
            || self.sdk.data_root().join("games").join(&version_id),
            Into::into,
        );
        let session = self
            .sdk
            .launch(&version_id, game_dir, LaunchRequest::new(auth))
            .await?;
        Ok(Arc::new(GameHandle::new(session)))
    }
}
