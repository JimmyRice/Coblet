use std::ffi::OsString;
use std::path::{Path, PathBuf};

use coblet_download::VerifyMode;
use coblet_launch_command::{LaunchAuth, Memory, QuickPlay, Resolution};
use coblet_runtime::ProcessSpec;
use coblet_types::ExtensionId;

use crate::DEFAULT_LOG_CAPACITY;

/// Command run before launch or after the game exits, in the game directory.
///
/// It gets `COBLET_GAME_DIR` in its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchHook {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
}

impl LaunchHook {
    /// Creates a hook.
    #[must_use]
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    pub(crate) fn spec(&self, game_directory: &Path) -> ProcessSpec {
        ProcessSpec::new(&self.program)
            .args(self.args.iter().cloned())
            .env("COBLET_GAME_DIR", game_directory.as_os_str())
            .cwd(game_directory)
    }
}

/// Per-launch choices: identity, Java, memory, window and hooks.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct LaunchRequest {
    /// Identity.
    pub auth: LaunchAuth,
    /// Java executable chosen by the user; checked against the version's needs.
    pub java: Option<PathBuf>,
    /// Heap limits.
    pub memory: Option<Memory>,
    /// Window size.
    pub resolution: Option<Resolution>,
    /// World or server to join after start.
    pub quick_play: Option<QuickPlay>,
    /// JVM arguments added after the version's.
    pub jvm_args: Vec<String>,
    /// Game arguments added after the version's.
    pub game_args: Vec<String>,
    /// Command the game runs under, such as `gamemoderun` or `prime-run`.
    pub wrapper: Vec<OsString>,
    /// Command run before the game starts; a failure stops the launch.
    pub pre_launch: Option<LaunchHook>,
    /// Command run after the game exits.
    pub post_exit: Option<LaunchHook>,
    /// How existing files are checked.
    pub verify: VerifyMode,
    /// Log lines kept for late subscribers.
    pub log_capacity: usize,
    /// `${launcher_name}` and `${launcher_version}`.
    pub launcher: Option<(String, String)>,
    /// Launch customizers to run, in order; none by default.
    pub customizers: Vec<ExtensionId>,
}

impl LaunchRequest {
    /// Creates a request with defaults for everything but the identity.
    #[must_use]
    pub fn new(auth: LaunchAuth) -> Self {
        Self {
            auth,
            java: None,
            memory: None,
            resolution: None,
            quick_play: None,
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            wrapper: Vec::new(),
            pre_launch: None,
            post_exit: None,
            verify: VerifyMode::Fast,
            log_capacity: DEFAULT_LOG_CAPACITY,
            launcher: None,
            customizers: Vec::new(),
        }
    }

    /// Uses this Java executable instead of searching.
    #[must_use]
    pub fn with_java(self, java: impl Into<PathBuf>) -> Self {
        Self {
            java: Some(java.into()),
            ..self
        }
    }

    /// Sets the heap limits.
    #[must_use]
    pub fn with_memory(self, memory: Memory) -> Self {
        Self {
            memory: Some(memory),
            ..self
        }
    }

    /// Sets the window size.
    #[must_use]
    pub fn with_resolution(self, resolution: Resolution) -> Self {
        Self {
            resolution: Some(resolution),
            ..self
        }
    }

    /// Runs the game under a wrapper command.
    #[must_use]
    pub fn with_wrapper(self, wrapper: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self {
            wrapper: wrapper.into_iter().map(Into::into).collect(),
            ..self
        }
    }

    /// Sets the file check mode.
    #[must_use]
    pub fn with_verify(self, verify: VerifyMode) -> Self {
        Self { verify, ..self }
    }

    /// Runs the launch customizer registered as `id`, after those already chosen.
    #[must_use]
    pub fn with_customizer(mut self, id: ExtensionId) -> Self {
        self.customizers.push(id);
        self
    }
}
