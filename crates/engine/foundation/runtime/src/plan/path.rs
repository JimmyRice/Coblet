use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Relative path with `/` separators that cannot leave its root.
///
/// Empty segments, `.`, `..`, backslashes, drive prefixes such as `C:` and
/// leading `/` are rejected, so joining it to a root directory always stays
/// inside that directory. Plans store paths this way so they stay valid
/// when serialized and resumed on another run.
///
/// # Examples
///
/// ```
/// use coblet_runtime::RelPath;
///
/// let path = RelPath::new("libraries/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar")?;
/// assert_eq!(path.file_name(), "lwjgl-3.3.3.jar");
/// assert!(RelPath::new("../etc/passwd").is_err());
/// # Ok::<(), coblet_runtime::RelPathError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

/// Reason a relative path was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RelPathError {
    /// The path is empty.
    #[error("relative path is empty")]
    Empty,
    /// The path starts at a root, a drive or a UNC prefix.
    #[error("path `{0}` is absolute")]
    Absolute(String),
    /// A segment is empty, `.` or `..`.
    #[error("path `{0}` has an empty, `.` or `..` segment")]
    BadSegment(String),
    /// The path contains a backslash, `:` or a control character.
    #[error("path {0:?} contains a backslash, `:` or a control character")]
    InvalidCharacter(String),
}

impl RelPath {
    /// Validates a relative path.
    ///
    /// # Errors
    ///
    /// Returns [`RelPathError`] when the path is empty, absolute, contains an
    /// empty, `.` or `..` segment, or contains `\`, `:` or a control character.
    pub fn new(path: impl Into<String>) -> Result<Self, RelPathError> {
        let path = path.into();
        if path.is_empty() {
            return Err(RelPathError::Empty);
        }
        if path.starts_with('/') {
            return Err(RelPathError::Absolute(path));
        }
        // `:` would allow `C:foo` drive-relative paths and NTFS alternate data streams on Windows.
        if path
            .chars()
            .any(|c| c == '\\' || c == ':' || c.is_control())
        {
            return Err(RelPathError::InvalidCharacter(path));
        }
        if path
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
        {
            return Err(RelPathError::BadSegment(path));
        }
        Ok(Self(path))
    }

    /// Returns the path with `/` separators.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the last segment.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// Appends one or more segments.
    ///
    /// # Errors
    ///
    /// Returns [`RelPathError`] when `child` is not itself a valid relative path.
    pub fn join(&self, child: &str) -> Result<Self, RelPathError> {
        let child = Self::new(child)?;
        Ok(Self(format!("{}/{}", self.0, child.0)))
    }

    /// Resolves the path below `root` using the platform separator.
    #[must_use]
    pub fn under(&self, root: &Path) -> PathBuf {
        self.0
            .split('/')
            .fold(root.to_path_buf(), |path, segment| path.join(segment))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RelPath").field(&self.0).finish()
    }
}

impl TryFrom<String> for RelPath {
    type Error = RelPathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RelPath> for String {
    fn from(value: RelPath) -> Self {
        value.0
    }
}

/// Directory a [`PlanPath`] is relative to; the executor supplies the actual locations.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathRoot {
    /// `data_root/.staging/<plan-id>/`, where every write lands first.
    Staging,
    /// The SDK data root, holding shared `libraries/`, `assets/`, `versions/` and caches.
    Data,
    /// The directory the Plan changes, such as an Instance directory or a server directory.
    Target,
}

/// Path used inside a Plan: a root plus a relative path below it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlanPath {
    /// The root directory.
    pub root: PathRoot,
    /// The path below the root.
    pub path: RelPath,
}

impl PlanPath {
    /// Builds a path below the staging directory.
    #[must_use]
    pub fn staging(path: RelPath) -> Self {
        Self {
            root: PathRoot::Staging,
            path,
        }
    }

    /// Builds a path below the data root.
    #[must_use]
    pub fn data(path: RelPath) -> Self {
        Self {
            root: PathRoot::Data,
            path,
        }
    }

    /// Builds a path below the target directory.
    #[must_use]
    pub fn target(path: RelPath) -> Self {
        Self {
            root: PathRoot::Target,
            path,
        }
    }
}

impl fmt::Display for PlanPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let root = match self.root {
            PathRoot::Staging => "staging",
            PathRoot::Data => "data",
            PathRoot::Target => "target",
        };
        write!(f, "{root}:{}", self.path)
    }
}

#[cfg(test)]
#[path = "../../tests/plan/path.rs"]
mod tests;
