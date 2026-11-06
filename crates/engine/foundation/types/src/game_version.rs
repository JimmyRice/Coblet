use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Longest accepted game version ID, in bytes.
const MAX_LEN: usize = 128;

/// Opaque Minecraft version ID as it appears in the version manifest, such as
/// `1.20.1`, `b1.7.3` or `26.1`.
///
/// The type deliberately implements neither `Ord` nor `PartialOrd`. Version
/// strings do not follow any comparable scheme (26.1 switched to year-based
/// numbers), so ordering comes from manifest order and release time only.
///
/// # Examples
///
/// ```
/// use coblet_types::GameVersionId;
///
/// let id: GameVersionId = "1.20.1".parse()?;
/// assert_eq!(id.as_str(), "1.20.1");
/// assert!(GameVersionId::new(" 1.20.1").is_err());
/// # Ok::<(), coblet_types::GameVersionIdError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GameVersionId(Arc<str>);

/// Reason a game version ID was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GameVersionIdError {
    /// The ID is empty.
    #[error("game version id is empty")]
    Empty,
    /// The ID starts or ends with whitespace.
    #[error("game version id `{0}` has leading or trailing whitespace")]
    SurroundingWhitespace(String),
    /// The ID contains a control character such as a newline.
    #[error("game version id {0:?} contains a control character")]
    ControlCharacter(String),
    /// The ID is longer than 128 bytes.
    #[error("game version id is {0} bytes long, the limit is 128")]
    TooLong(usize),
}

impl GameVersionId {
    /// Validates a version ID without interpreting it.
    ///
    /// Inner spaces are allowed because the manifest contains IDs such as
    /// `1.14 Pre-Release 1`.
    ///
    /// # Errors
    ///
    /// Returns [`GameVersionIdError`] when the ID is empty, has surrounding
    /// whitespace, contains a control character or exceeds 128 bytes.
    pub fn new(id: impl Into<String>) -> Result<Self, GameVersionIdError> {
        let id = id.into();
        if id.is_empty() {
            return Err(GameVersionIdError::Empty);
        }
        if id.len() > MAX_LEN {
            return Err(GameVersionIdError::TooLong(id.len()));
        }
        if id.trim() != id {
            return Err(GameVersionIdError::SurroundingWhitespace(id));
        }
        if id.chars().any(char::is_control) {
            return Err(GameVersionIdError::ControlCharacter(id));
        }
        Ok(Self(id.into()))
    }

    /// Returns the ID exactly as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GameVersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for GameVersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("GameVersionId")
            .field(&self.as_str())
            .finish()
    }
}

impl AsRef<str> for GameVersionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for GameVersionId {
    type Err = GameVersionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for GameVersionId {
    type Error = GameVersionIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for GameVersionId {
    type Error = GameVersionIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<GameVersionId> for String {
    fn from(id: GameVersionId) -> Self {
        id.0.as_ref().to_owned()
    }
}

/// Release channel of a game version, from the manifest `type` field.
///
/// Values other than the five known ones are kept in [`VersionType::Other`].
/// Parsing always maps a known string to its named variant, so
/// `VersionType::from("release")` is [`VersionType::Release`], never `Other`.
///
/// # Examples
///
/// ```
/// use coblet_types::VersionType;
///
/// assert_eq!(VersionType::from("old_beta"), VersionType::OldBeta);
/// assert_eq!(VersionType::from("pending").as_str(), "pending");
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum VersionType {
    /// A full release such as `1.20.1` or `26.1`.
    Release,
    /// A development snapshot or pre-release.
    Snapshot,
    /// A Beta version such as `b1.7.3`.
    OldBeta,
    /// An Alpha, Infdev, Indev or Classic version.
    OldAlpha,
    /// An experimental build missing from the official manifest, such as a Combat Test.
    Experiment,
    /// A type the engine does not know, stored verbatim.
    Other(String),
}

impl VersionType {
    /// Returns the manifest spelling, such as `old_alpha`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Release => "release",
            Self::Snapshot => "snapshot",
            Self::OldBeta => "old_beta",
            Self::OldAlpha => "old_alpha",
            Self::Experiment => "experiment",
            Self::Other(other) => other,
        }
    }
}

impl fmt::Display for VersionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for VersionType {
    fn from(value: &str) -> Self {
        match value {
            "release" => Self::Release,
            "snapshot" => Self::Snapshot,
            "old_beta" => Self::OldBeta,
            "old_alpha" => Self::OldAlpha,
            "experiment" => Self::Experiment,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl From<String> for VersionType {
    fn from(value: String) -> Self {
        match Self::from(value.as_str()) {
            Self::Other(_) => Self::Other(value),
            known => known,
        }
    }
}

impl From<VersionType> for String {
    fn from(value: VersionType) -> Self {
        match value {
            VersionType::Other(other) => other,
            known => known.as_str().to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/game_version.rs"]
mod tests;
