use std::path::PathBuf;
use std::sync::Arc;

use crate::{
    AssetsLocation, CommandError, CommandProfile, JavaExecutable, LaunchAuth, LaunchCommand,
    MainClass, Memory, PlaceholderResolver, QuickPlay, Resolution, TemplateArg, UnknownPlaceholder,
};

/// Per-launch settings that [`LaunchCommand::from_profile`] combines with a [`CommandProfile`].
///
/// # Examples
///
/// ```
/// use coblet_launch_command::{
///     CommandProfile, JavaExecutable, LaunchAuth, LaunchCommand, LaunchOptions,
/// };
/// use coblet_types::PlayerName;
///
/// let mut profile = CommandProfile::new("1.12.2", "net.minecraft.client.main.Main");
/// profile.assets_index_name = "1.12".to_owned();
/// profile.game_args = vec!["--username".into(), "${auth_player_name}".into()];
/// let options = LaunchOptions::new(
///     JavaExecutable::new("/opt/jdk-8/bin/java"),
///     LaunchAuth::offline(PlayerName::new("Alex")?),
///     "/games/minecraft",
/// )
/// .with_assets_root("/data/assets");
/// let command = LaunchCommand::from_profile(&profile, &options)?;
/// assert_eq!(command.to_string(), "/opt/jdk-8/bin/java net.minecraft.client.main.Main --username Alex");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[non_exhaustive]
#[derive(Clone)]
pub struct LaunchOptions {
    /// Java executable.
    pub java: JavaExecutable,
    /// Identity.
    pub auth: LaunchAuth,
    /// Game directory, also the working directory.
    pub game_directory: PathBuf,
    /// `${assets_root}`.
    pub assets_root: Option<PathBuf>,
    /// `${game_assets}`; by default `<assets_root>/virtual/<index>`.
    pub game_assets: Option<PathBuf>,
    /// `${natives_directory}`.
    pub natives_directory: Option<PathBuf>,
    /// `${library_directory}`.
    pub library_directory: Option<PathBuf>,
    /// `${classpath_separator}`; by default that of the host.
    pub classpath_separator: Option<String>,
    /// Heap limits.
    pub memory: Option<Memory>,
    /// Window size.
    pub resolution: Option<Resolution>,
    /// Quick Play target.
    pub quick_play: Option<QuickPlay>,
    /// `${launcher_name}` and `${launcher_version}`.
    pub launcher: Option<(String, String)>,
    /// `${user_type}` override.
    pub user_type: Option<String>,
    /// JVM arguments appended after the profile's.
    pub extra_jvm_args: Vec<TemplateArg>,
    /// Game arguments appended after the profile's.
    pub extra_game_args: Vec<TemplateArg>,
    /// Explicit feature values.
    pub features: Vec<(String, bool)>,
    /// Extra placeholder resolvers.
    pub resolvers: Vec<Arc<dyn PlaceholderResolver>>,
    /// Unknown placeholder policy.
    pub unknown: UnknownPlaceholder,
    /// Argument file path and threshold; see [`LaunchCommandBuilder::argfile`](crate::LaunchCommandBuilder::argfile).
    pub argfile: Option<(PathBuf, usize)>,
}

impl std::fmt::Debug for LaunchOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchOptions")
            .field("java", &self.java)
            .field("auth", &self.auth)
            .field("game_directory", &self.game_directory)
            .finish_non_exhaustive()
    }
}

impl LaunchOptions {
    /// Creates options with the required values.
    #[must_use]
    pub fn new(java: JavaExecutable, auth: LaunchAuth, game_directory: impl Into<PathBuf>) -> Self {
        Self {
            java,
            auth,
            game_directory: game_directory.into(),
            assets_root: None,
            game_assets: None,
            natives_directory: None,
            library_directory: None,
            classpath_separator: None,
            memory: None,
            resolution: None,
            quick_play: None,
            launcher: None,
            user_type: None,
            extra_jvm_args: Vec::new(),
            extra_game_args: Vec::new(),
            features: Vec::new(),
            resolvers: Vec::new(),
            unknown: UnknownPlaceholder::Error,
            argfile: None,
        }
    }

    /// Sets `${assets_root}`.
    #[must_use]
    pub fn with_assets_root(self, path: impl Into<PathBuf>) -> Self {
        Self {
            assets_root: Some(path.into()),
            ..self
        }
    }

    /// Sets `${natives_directory}`.
    #[must_use]
    pub fn with_natives_directory(self, path: impl Into<PathBuf>) -> Self {
        Self {
            natives_directory: Some(path.into()),
            ..self
        }
    }

    /// Sets `${library_directory}`.
    #[must_use]
    pub fn with_library_directory(self, path: impl Into<PathBuf>) -> Self {
        Self {
            library_directory: Some(path.into()),
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

    /// Sets the launcher name and version.
    #[must_use]
    pub fn with_launcher(self, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            launcher: Some((name.into(), version.into())),
            ..self
        }
    }

    /// Enables the argument file fallback.
    #[must_use]
    pub fn with_argfile(self, path: impl Into<PathBuf>, threshold: usize) -> Self {
        Self {
            argfile: Some((path.into(), threshold)),
            ..self
        }
    }
}

impl LaunchCommand {
    /// Builds the command for `profile` with per-launch `options`.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] for unknown or unterminated placeholders.
    pub fn from_profile(
        profile: &CommandProfile,
        options: &LaunchOptions,
    ) -> Result<Self, CommandError> {
        let mut builder = Self::builder()
            .java(options.java.clone())
            .main_class(MainClass::new(profile.main_class.clone()))
            .auth(options.auth.clone())
            .classpath(profile.classpath.iter().cloned())
            .game_directory(options.game_directory.clone())
            .jvm_args(profile.jvm_args.iter().cloned())
            .jvm_args(options.extra_jvm_args.iter().cloned())
            .game_args(profile.game_args.iter().cloned())
            .game_args(options.extra_game_args.iter().cloned())
            .version(profile.version_name.clone(), profile.version_type.clone())
            .env(profile.env.iter().cloned())
            .unknown_placeholders(options.unknown);
        if let Some(root) = &options.assets_root {
            let mut assets = AssetsLocation::new(root.clone(), profile.assets_index_name.clone());
            if let Some(game_assets) = &options.game_assets {
                assets = assets.with_game_assets(game_assets.clone());
            }
            builder = builder.assets(assets);
        }
        if let Some(path) = &options.natives_directory {
            builder = builder.natives_directory(path.clone());
        }
        if let Some(path) = &options.library_directory {
            builder = builder.library_directory(path.clone());
        }
        if let Some(separator) = &options.classpath_separator {
            builder = builder.classpath_separator(separator.clone());
        }
        if let Some(memory) = options.memory {
            builder = builder.memory(memory);
        }
        if let Some(resolution) = options.resolution {
            builder = builder.resolution(resolution);
        }
        if let Some(quick_play) = &options.quick_play {
            builder = builder.quick_play(quick_play.clone());
        }
        if let Some((name, version)) = &options.launcher {
            builder = builder.launcher(name.clone(), version.clone());
        }
        if let Some(user_type) = &options.user_type {
            builder = builder.user_type(user_type.clone());
        }
        for (name, value) in &options.features {
            builder = builder.feature(name.clone(), *value);
        }
        for resolver in &options.resolvers {
            builder = builder.placeholder_resolver(Arc::clone(resolver));
        }
        if let Some((path, threshold)) = &options.argfile {
            builder = builder.argfile(path.clone(), *threshold);
        }
        builder.build()
    }
}
