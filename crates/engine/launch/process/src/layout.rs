use std::path::{Path, PathBuf};

use coblet_types::GameVersionId;

/// Where game files live: the shared data root and the game directory.
///
/// The data root holds `libraries/`, `assets/`, `versions/<id>/<id>.jar`
/// and `natives/<id>/`, shared by every instance. The game directory holds
/// saves, options and logs and is the working directory of the game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameLayout {
    data_root: PathBuf,
    game_directory: PathBuf,
}

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

impl GameLayout {
    /// Creates a layout; relative paths are made absolute against the
    /// current directory, because the game runs in the game directory.
    #[must_use]
    pub fn new(data_root: impl Into<PathBuf>, game_directory: impl Into<PathBuf>) -> Self {
        Self {
            data_root: absolute(data_root.into()),
            game_directory: absolute(game_directory.into()),
        }
    }

    /// Returns the data root.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Returns the game directory.
    #[must_use]
    pub fn game_directory(&self) -> &Path {
        &self.game_directory
    }

    /// Returns `libraries/`.
    #[must_use]
    pub fn libraries(&self) -> PathBuf {
        self.data_root.join("libraries")
    }

    /// Returns a library file from its `/`-separated relative path.
    #[must_use]
    pub fn library(&self, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(self.libraries(), |path, part| path.join(part))
    }

    /// Returns `assets/`.
    #[must_use]
    pub fn assets(&self) -> PathBuf {
        self.data_root.join("assets")
    }

    /// Returns `assets/indexes/<id>.json`.
    #[must_use]
    pub fn asset_index(&self, id: &str) -> PathBuf {
        self.assets().join("indexes").join(format!("{id}.json"))
    }

    /// Returns the object file of an asset from its `<xx>/<hash>` path.
    #[must_use]
    pub fn asset_object(&self, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(self.assets().join("objects"), |path, part| path.join(part))
    }

    /// Returns `assets/virtual/<id>/`, the by-name copy old indexes ask for.
    #[must_use]
    pub fn virtual_assets(&self, id: &str) -> PathBuf {
        self.assets().join("virtual").join(id)
    }

    /// Returns `<game directory>/resources/`, where the oldest versions read assets.
    #[must_use]
    pub fn resources(&self) -> PathBuf {
        self.game_directory.join("resources")
    }

    /// Returns `assets/log_configs/<file>`.
    #[must_use]
    pub fn log_config(&self, file: &str) -> PathBuf {
        self.assets().join("log_configs").join(file)
    }

    /// Returns `versions/<id>/<id>.jar`.
    #[must_use]
    pub fn client_jar(&self, version: &GameVersionId) -> PathBuf {
        let id = version.as_str();
        self.data_root
            .join("versions")
            .join(id)
            .join(format!("{id}.jar"))
    }

    /// Returns `natives/<id>/`.
    #[must_use]
    pub fn natives(&self, version: &GameVersionId) -> PathBuf {
        self.data_root.join("natives").join(version.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::GameLayout;

    #[test]
    fn makes_relative_paths_absolute() {
        let layout = GameLayout::new("data", "games/a");
        assert!(layout.data_root().is_absolute());
        assert!(layout.game_directory().ends_with("games/a"));
        assert_eq!(
            layout.library("a/b/c.jar"),
            layout
                .data_root()
                .join("libraries")
                .join("a")
                .join("b")
                .join("c.jar")
        );
    }
}
