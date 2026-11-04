use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Shortest name the Minecraft account system accepts.
const MIN_LEN: usize = 3;
/// Longest name the Minecraft account system accepts.
const MAX_LEN: usize = 16;

/// Minecraft player name: 3 to 16 characters from `A-Z`, `a-z`, `0-9` and `_`.
///
/// The rule is the one Mojang applies when a name is chosen or changed. Case
/// is kept because the game displays names as written; equality is exact.
///
/// # Examples
///
/// ```
/// use coblet_types::PlayerName;
///
/// let steve = PlayerName::new("Steve")?;
/// assert_eq!(steve.as_str(), "Steve");
/// assert!(PlayerName::new("no spaces").is_err());
/// # Ok::<(), coblet_types::PlayerNameError>(())
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PlayerName(String);

/// Reason a player name was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlayerNameError {
    /// The name is shorter than 3 or longer than 16 characters.
    #[error("player name `{0}` must be 3 to 16 characters long")]
    Length(String),
    /// The name contains a character outside `A-Z a-z 0-9 _`.
    #[error("player name `{name}` contains `{character}`, allowed are A-Z a-z 0-9 _")]
    InvalidCharacter {
        /// The rejected name.
        name: String,
        /// The first character that is not allowed.
        character: char,
    },
}

impl PlayerName {
    /// Validates a player name.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerNameError::Length`] or [`PlayerNameError::InvalidCharacter`].
    pub fn new(name: impl Into<String>) -> Result<Self, PlayerNameError> {
        let name = name.into();
        if let Some(character) = name
            .chars()
            .find(|&c| !(c.is_ascii_alphanumeric() || c == '_'))
        {
            return Err(PlayerNameError::InvalidCharacter { name, character });
        }
        // Every accepted character is ASCII, so the byte length is the character count.
        if !(MIN_LEN..=MAX_LEN).contains(&name.len()) {
            return Err(PlayerNameError::Length(name));
        }
        Ok(Self(name))
    }

    /// Returns the name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlayerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for PlayerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PlayerName").field(&self.0).finish()
    }
}

impl AsRef<str> for PlayerName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for PlayerName {
    type Err = PlayerNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for PlayerName {
    type Error = PlayerNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PlayerName> for String {
    fn from(value: PlayerName) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::{PlayerName, PlayerNameError};

    #[test]
    fn accepts_valid_names() {
        for name in ["abc", "Steve", "Notch_2009", "ABCDEFGHIJKLMNOP"] {
            assert_eq!(PlayerName::new(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn rejects_invalid_names() {
        assert_eq!(
            PlayerName::new("ab"),
            Err(PlayerNameError::Length("ab".to_owned()))
        );
        assert!(matches!(
            PlayerName::new("a".repeat(17)),
            Err(PlayerNameError::Length(_))
        ));
        assert!(matches!(
            PlayerName::new("Ste-ve"),
            Err(PlayerNameError::InvalidCharacter { character: '-', .. })
        ));
        assert!(matches!(
            PlayerName::new("南瞳大冒险"),
            Err(PlayerNameError::InvalidCharacter {
                character: '南',
                ..
            })
        ));
        assert!(serde_json::from_str::<PlayerName>(r#""x y z""#).is_err());
    }
}
