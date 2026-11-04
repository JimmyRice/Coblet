use std::fmt;

use serde::{Deserialize, Serialize};

use super::name::{LibcName, PlatformNameError};

/// C library of the platform, which decides which native builds can load.
///
/// # Examples
///
/// ```
/// use coblet_types::Libc;
///
/// assert_eq!("musl".parse::<Libc>()?, Libc::Musl);
/// # Ok::<(), coblet_types::PlatformNameError>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Libc {
    /// GNU C library.
    Glibc,
    /// musl libc.
    Musl,
    /// Another C library, kept by name.
    Other(LibcName),
}

impl Libc {
    /// Returns the name, such as `glibc`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Glibc => "glibc",
            Self::Musl => "musl",
            Self::Other(name) => name.as_str(),
        }
    }
}

impl From<LibcName> for Libc {
    /// Maps `glibc` (or `gnu`) and `musl` to their variants and keeps the rest as [`Libc::Other`].
    fn from(name: LibcName) -> Self {
        match name.as_str() {
            "glibc" | "gnu" => Self::Glibc,
            "musl" => Self::Musl,
            _ => Self::Other(name),
        }
    }
}

impl std::str::FromStr for Libc {
    type Err = PlatformNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        LibcName::new(s).map(Self::from)
    }
}

impl TryFrom<String> for Libc {
    type Error = PlatformNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Libc> for String {
    fn from(value: Libc) -> Self {
        value.as_str().to_owned()
    }
}

impl fmt::Display for Libc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
