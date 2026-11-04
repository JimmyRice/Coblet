use std::path::PathBuf;

use coblet_types::MiB;

use crate::CommandError;

/// Path of the `java` executable that runs the game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaExecutable(PathBuf);

impl JavaExecutable {
    /// Wraps a path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// Returns the path.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

/// Fully qualified Java class with the `main` method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainClass(String);

impl MainClass {
    /// Wraps a class name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Returns the class name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where the game finds its assets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetsLocation {
    root: PathBuf,
    index: String,
    game_assets: Option<PathBuf>,
}

impl AssetsLocation {
    /// Creates a location from the assets root (`${assets_root}`) and the index ID.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, index: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            index: index.into(),
            game_assets: None,
        }
    }

    /// Sets `${game_assets}`, the directory old versions read assets from by
    /// name; by default `<root>/virtual/<index>`.
    #[must_use]
    pub fn with_game_assets(self, path: impl Into<PathBuf>) -> Self {
        Self {
            game_assets: Some(path.into()),
            ..self
        }
    }

    /// Returns the assets root.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Returns the index ID.
    #[must_use]
    pub fn index(&self) -> &str {
        &self.index
    }

    /// Returns `${game_assets}`.
    #[must_use]
    pub fn game_assets(&self) -> PathBuf {
        self.game_assets
            .clone()
            .unwrap_or_else(|| self.root.join("virtual").join(&self.index))
    }
}

/// Java heap limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    min: MiB,
    max: MiB,
}

impl Memory {
    /// Creates heap limits.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::Memory`] when `min` exceeds `max` or `max` is zero.
    pub fn new(min: MiB, max: MiB) -> Result<Self, CommandError> {
        if min.0 > max.0 || max.0 == 0 {
            return Err(CommandError::Memory { min, max });
        }
        Ok(Self { min, max })
    }

    /// Returns the initial heap size.
    #[must_use]
    pub fn min(self) -> MiB {
        self.min
    }

    /// Returns the maximum heap size.
    #[must_use]
    pub fn max(self) -> MiB {
        self.max
    }
}

/// Window size passed with `has_custom_resolution`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// World or server to join right after start (Quick Play).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuickPlay {
    /// A singleplayer world by folder name.
    Singleplayer(String),
    /// A server address such as `play.example.net:25565`.
    Multiplayer(String),
    /// A Realm by ID.
    Realms(String),
}
