use std::fmt;

use serde::{Deserialize, Serialize};

use super::name::{OsName, PlatformNameError};

/// Operating system family.
///
/// Names follow [`std::env::consts::OS`]. A system without its own variant is
/// kept as [`OsFamily::Other`] so Platform Profiles can still match it.
/// Mojang's `osx` / `windows` / `linux` rule names are not part of this type:
/// Platform Profiles map a family onto them.
///
/// # Examples
///
/// ```
/// use coblet_types::OsFamily;
///
/// assert_eq!("freebsd".parse::<OsFamily>()?, OsFamily::FreeBsd);
/// assert_eq!("Haiku".parse::<OsFamily>()?.as_str(), "haiku");
/// # Ok::<(), coblet_types::PlatformNameError>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum OsFamily {
    /// Microsoft Windows.
    Windows,
    /// Apple macOS.
    MacOs,
    /// Linux, any distribution.
    Linux,
    /// FreeBSD.
    FreeBsd,
    /// OpenBSD.
    OpenBsd,
    /// NetBSD.
    NetBsd,
    /// DragonFly BSD.
    DragonFly,
    /// illumos.
    Illumos,
    /// Android.
    Android,
    /// A system the engine does not know, kept by name.
    Other(OsName),
}

impl OsFamily {
    /// Returns the [`std::env::consts::OS`] spelling, such as `macos`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Windows => "windows",
            Self::MacOs => "macos",
            Self::Linux => "linux",
            Self::FreeBsd => "freebsd",
            Self::OpenBsd => "openbsd",
            Self::NetBsd => "netbsd",
            Self::DragonFly => "dragonfly",
            Self::Illumos => "illumos",
            Self::Android => "android",
            Self::Other(name) => name.as_str(),
        }
    }
}

impl From<OsName> for OsFamily {
    /// Maps a known name to its variant and keeps the rest as [`OsFamily::Other`].
    fn from(name: OsName) -> Self {
        match name.as_str() {
            "windows" => Self::Windows,
            "macos" => Self::MacOs,
            "linux" => Self::Linux,
            "freebsd" => Self::FreeBsd,
            "openbsd" => Self::OpenBsd,
            "netbsd" => Self::NetBsd,
            "dragonfly" => Self::DragonFly,
            "illumos" => Self::Illumos,
            "android" => Self::Android,
            _ => Self::Other(name),
        }
    }
}

impl std::str::FromStr for OsFamily {
    type Err = PlatformNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OsName::new(s).map(Self::from)
    }
}

impl TryFrom<String> for OsFamily {
    type Error = PlatformNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<OsFamily> for String {
    fn from(value: OsFamily) -> Self {
        value.as_str().to_owned()
    }
}

impl fmt::Display for OsFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "../../tests/platform/os.rs"]
mod tests;
