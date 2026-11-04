use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Longest accepted extension ID, in bytes.
const MAX_LEN: usize = 128;

/// Namespaced key of a registered extension, such as `coblet:fabric` or `acme:my-loader`.
///
/// Built-in extensions use the `coblet` namespace. The namespace may contain
/// `a-z`, `0-9`, `_`, `-` and `.`; the name may also contain `/`. Both parts
/// are non-empty and the whole ID is at most 128 bytes.
///
/// Cloning is cheap: the string is shared behind an `Arc`.
///
/// # Examples
///
/// ```
/// use coblet_types::ExtensionId;
///
/// let id = ExtensionId::new("coblet:fabric")?;
/// assert_eq!(id.namespace(), "coblet");
/// assert_eq!(id.name(), "fabric");
/// assert!(ExtensionId::new("Fabric").is_err());
/// # Ok::<(), coblet_types::ExtensionIdError>(())
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExtensionId {
    value: Arc<str>,
    colon: usize,
}

/// Reason an extension ID was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionIdError {
    /// The ID has no `:` separating namespace and name.
    #[error("extension id `{0}` has no `namespace:` prefix")]
    MissingNamespace(String),
    /// The namespace or name is empty.
    #[error("extension id `{0}` has an empty namespace or name")]
    EmptyPart(String),
    /// The ID contains a character outside the allowed set.
    #[error(
        "extension id `{id}` contains `{character}`, allowed are a-z 0-9 _ - . and / in the name"
    )]
    InvalidCharacter {
        /// The rejected ID.
        id: String,
        /// The first character that is not allowed.
        character: char,
    },
    /// The ID is longer than 128 bytes.
    #[error("extension id is {0} bytes long, the limit is 128")]
    TooLong(usize),
}

impl ExtensionId {
    /// Parses and validates an extension ID.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionIdError`] when the ID has no namespace, has an empty
    /// part, contains a character outside the allowed set or exceeds 128 bytes.
    pub fn new(id: impl Into<String>) -> Result<Self, ExtensionIdError> {
        let id = id.into();
        if id.len() > MAX_LEN {
            return Err(ExtensionIdError::TooLong(id.len()));
        }
        let Some(colon) = id.find(':') else {
            return Err(ExtensionIdError::MissingNamespace(id));
        };
        let (namespace, name) = (&id[..colon], &id[colon + 1..]);
        if namespace.is_empty() || name.is_empty() {
            return Err(ExtensionIdError::EmptyPart(id));
        }
        let invalid = namespace
            .chars()
            .find(|&c| !is_namespace_char(c))
            .or_else(|| name.chars().find(|&c| !is_name_char(c)));
        if let Some(character) = invalid {
            return Err(ExtensionIdError::InvalidCharacter { id, character });
        }
        Ok(Self {
            value: id.into(),
            colon,
        })
    }

    /// Returns the part before the colon, such as `coblet`.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.value[..self.colon]
    }

    /// Returns the part after the colon, such as `fabric`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.value[self.colon + 1..]
    }

    /// Returns the full ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

fn is_namespace_char(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '.')
}

fn is_name_char(c: char) -> bool {
    is_namespace_char(c) || c == '/'
}

impl fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

impl fmt::Debug for ExtensionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ExtensionId").field(&self.as_str()).finish()
    }
}

impl AsRef<str> for ExtensionId {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

impl FromStr for ExtensionId {
    type Err = ExtensionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for ExtensionId {
    type Error = ExtensionIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ExtensionId {
    type Error = ExtensionIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ExtensionId> for String {
    fn from(id: ExtensionId) -> Self {
        id.value.as_ref().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{ExtensionId, ExtensionIdError};

    #[test]
    fn splits_namespace_and_name() {
        let id = ExtensionId::new("acme:loaders/my-loader.v2").unwrap();
        assert_eq!(id.namespace(), "acme");
        assert_eq!(id.name(), "loaders/my-loader.v2");
        assert_eq!(id.to_string(), "acme:loaders/my-loader.v2");
    }

    #[test]
    fn rejects_malformed_ids() {
        assert_eq!(
            ExtensionId::new("fabric"),
            Err(ExtensionIdError::MissingNamespace("fabric".to_owned()))
        );
        assert_eq!(
            ExtensionId::new(":fabric"),
            Err(ExtensionIdError::EmptyPart(":fabric".to_owned()))
        );
        assert_eq!(
            ExtensionId::new("coblet:"),
            Err(ExtensionIdError::EmptyPart("coblet:".to_owned()))
        );
        assert_eq!(
            ExtensionId::new("Coblet:fabric"),
            Err(ExtensionIdError::InvalidCharacter {
                id: "Coblet:fabric".to_owned(),
                character: 'C'
            })
        );
        assert!(matches!(
            ExtensionId::new("a/b:c"),
            Err(ExtensionIdError::InvalidCharacter { character: '/', .. })
        ));
        assert!(matches!(
            ExtensionId::new("coblet:a:b"),
            Err(ExtensionIdError::InvalidCharacter { character: ':', .. })
        ));
        assert_eq!(
            ExtensionId::new(format!("x:{}", "a".repeat(127))),
            Err(ExtensionIdError::TooLong(129))
        );
    }

    #[test]
    fn round_trips_through_serde() {
        let id = ExtensionId::new("coblet:forge").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""coblet:forge""#);
        assert_eq!(serde_json::from_str::<ExtensionId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<ExtensionId>(r#""forge""#).is_err());
    }
}
