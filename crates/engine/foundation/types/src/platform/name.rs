use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Longest accepted platform name, in bytes.
const MAX_LEN: usize = 64;

/// Reason a platform name was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlatformNameError {
    /// The name is empty.
    #[error("platform name is empty")]
    Empty,
    /// The name contains a character other than ASCII letters, digits, `_`, `-` and `.`.
    #[error("platform name `{name}` contains `{character}`, allowed are a-z 0-9 _ - .")]
    InvalidCharacter {
        /// The rejected name.
        name: String,
        /// The first character that is not allowed.
        character: char,
    },
    /// The name is longer than 64 bytes.
    #[error("platform name is {0} bytes long, the limit is 64")]
    TooLong(usize),
}

/// Validated, lowercased identifier shared by the platform name newtypes.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Name(Arc<str>);

impl Name {
    pub(super) fn new(name: &str) -> Result<Self, PlatformNameError> {
        if name.is_empty() {
            return Err(PlatformNameError::Empty);
        }
        if name.len() > MAX_LEN {
            return Err(PlatformNameError::TooLong(name.len()));
        }
        if let Some(character) = name
            .chars()
            .find(|&c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        {
            return Err(PlatformNameError::InvalidCharacter {
                name: name.to_owned(),
                character,
            });
        }
        Ok(Self(name.to_ascii_lowercase().into()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Declares a public newtype around [`Name`] with the usual conversions.
macro_rules! platform_name {
    ($(#[$doc:meta])* $ty:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $ty(Name);

        impl $ty {
            /// Validates the name and converts it to lowercase.
            ///
            /// # Errors
            ///
            /// Returns [`PlatformNameError`] when the name is empty, longer than
            /// 64 bytes or contains a character other than ASCII letters,
            /// digits, `_`, `-` and `.`.
            pub fn new(name: &str) -> Result<Self, PlatformNameError> {
                Name::new(name).map(Self)
            }

            /// Returns the lowercased name.
            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($ty)).field(&self.as_str()).finish()
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::str::FromStr for $ty {
            type Err = PlatformNameError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $ty {
            type Error = PlatformNameError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(&value)
            }
        }

        impl From<$ty> for String {
            fn from(value: $ty) -> Self {
                value.as_str().to_owned()
            }
        }
    };
}

platform_name!(
    /// Identifier of an operating system the engine has no variant for, such as `haiku`.
    OsName
);

platform_name!(
    /// Identifier of a CPU architecture the engine has no variant for, such as `sparc64`.
    ArchName
);

platform_name!(
    /// Identifier of a C library other than glibc and musl, such as `bionic`.
    LibcName
);

platform_name!(
    /// Free-form platform tag that Platform Profiles can match on, such as `wayland` or `rosetta`.
    PlatformTrait
);

#[cfg(test)]
mod tests {
    use super::{OsName, PlatformNameError, PlatformTrait};

    #[test]
    fn lowercases_valid_names() {
        assert_eq!(OsName::new("Haiku").unwrap().as_str(), "haiku");
        assert_eq!(
            PlatformTrait::new("gpu.nvidia-prime_1").unwrap().as_str(),
            "gpu.nvidia-prime_1"
        );
    }

    #[test]
    fn rejects_invalid_names() {
        assert_eq!(OsName::new(""), Err(PlatformNameError::Empty));
        assert!(matches!(
            OsName::new("mac os"),
            Err(PlatformNameError::InvalidCharacter { character: ' ', .. })
        ));
        assert_eq!(
            OsName::new(&"a".repeat(65)),
            Err(PlatformNameError::TooLong(65))
        );
    }
}
