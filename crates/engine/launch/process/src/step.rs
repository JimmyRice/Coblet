use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use coblet_catalog::CatalogContext;
use coblet_download::Downloader;
use coblet_java::{JavaInstall, JavaInstaller, JavaLocators, SearchPaths};
use coblet_launch_command::LaunchCommand;
use coblet_platform::EffectiveLaunchProfile;
use coblet_runtime::{BoxFuture, CancellationToken, EventBus, ProcessSpawner, Storage};
use coblet_types::OsFamily;

use crate::{GameLayout, GameSession, LaunchError, LaunchRequest};

/// One stage of a launch, such as selecting Java or extracting natives.
///
/// Steps run in order on a shared [`LaunchContext`]; each reads what earlier
/// steps left there and adds its own results.
pub trait LaunchStep: Send + Sync + 'static {
    /// Returns the step name, such as `select-java`.
    fn name(&self) -> &str;

    /// Runs the step.
    fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>>;
}

/// Services a launch uses.
#[non_exhaustive]
#[derive(Clone)]
pub struct LaunchServices {
    /// File access.
    pub storage: Arc<dyn Storage>,
    /// Process spawning.
    pub spawner: Arc<dyn ProcessSpawner>,
    /// Game file downloads.
    pub downloader: Downloader,
    /// Metadata fetches, such as `.sha1` files and the Java runtime index.
    pub catalog: Arc<CatalogContext>,
    /// Where to look for installed Java.
    pub java_locators: JavaLocators,
    /// Java runtime installer and its providers; `None` disables runtime downloads.
    pub java_installer: Option<JavaInstaller>,
    /// Operating system of this machine.
    pub os: OsFamily,
    /// Event stream that receives game log lines.
    pub events: Option<EventBus>,
}

impl LaunchServices {
    /// Creates services without runtime downloads or events.
    #[must_use]
    pub fn new(
        storage: Arc<dyn Storage>,
        spawner: Arc<dyn ProcessSpawner>,
        downloader: Downloader,
        catalog: Arc<CatalogContext>,
        os: OsFamily,
    ) -> Self {
        Self {
            storage,
            spawner,
            downloader,
            catalog,
            java_locators: JavaLocators::builtin(SearchPaths::from_env()),
            java_installer: None,
            os,
            events: None,
        }
    }

    /// Sets where to look for installed Java.
    #[must_use]
    pub fn with_java_locators(self, java_locators: JavaLocators) -> Self {
        Self {
            java_locators,
            ..self
        }
    }

    /// Allows downloading Java runtimes from the installer's providers.
    #[must_use]
    pub fn with_java_installer(self, installer: JavaInstaller) -> Self {
        Self {
            java_installer: Some(installer),
            ..self
        }
    }

    /// Publishes game log lines to `events`.
    #[must_use]
    pub fn with_events(self, events: EventBus) -> Self {
        Self {
            events: Some(events),
            ..self
        }
    }
}

impl fmt::Debug for LaunchServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchServices")
            .field("os", &self.os)
            .field("java_locators", &self.java_locators)
            .finish_non_exhaustive()
    }
}

/// State shared by the steps of one launch.
#[non_exhaustive]
#[derive(Debug)]
pub struct LaunchContext {
    /// Services.
    pub services: LaunchServices,
    /// The version adapted to this platform.
    pub profile: EffectiveLaunchProfile,
    /// File locations.
    pub layout: GameLayout,
    /// Per-launch choices.
    pub request: LaunchRequest,
    /// Cancels the launch before the game starts.
    pub cancel: CancellationToken,
    /// Java chosen by `select-java`.
    pub java: Option<JavaInstall>,
    /// `${game_assets}`, set by `verify-files` for indexes that need it.
    pub game_assets: Option<PathBuf>,
    /// Natives directory prepared by `prepare-natives`.
    pub natives_directory: Option<PathBuf>,
    /// Client jar chosen by `client-jar`.
    pub client_jar: Option<PathBuf>,
    /// JVM arguments added by steps, such as the log configuration.
    pub jvm_args: Vec<String>,
    /// Command built by `build-command`.
    pub command: Option<LaunchCommand>,
    /// Session started by `spawn`.
    pub session: Option<GameSession>,
}

impl LaunchContext {
    /// Creates a context for one launch.
    #[must_use]
    pub fn new(
        services: LaunchServices,
        profile: EffectiveLaunchProfile,
        layout: GameLayout,
        request: LaunchRequest,
    ) -> Self {
        Self {
            services,
            profile,
            layout,
            request,
            cancel: CancellationToken::new(),
            java: None,
            game_assets: None,
            natives_directory: None,
            client_jar: None,
            jvm_args: Vec::new(),
            command: None,
            session: None,
        }
    }

    /// Uses `cancel` to stop the launch.
    #[must_use]
    pub fn with_cancel(self, cancel: CancellationToken) -> Self {
        Self { cancel, ..self }
    }
}
