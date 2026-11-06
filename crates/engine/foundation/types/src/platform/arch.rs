use std::fmt;

use serde::{Deserialize, Serialize};

use super::name::{ArchName, PlatformNameError};

/// CPU architecture.
///
/// Names follow [`std::env::consts::ARCH`], except that little-endian 64-bit
/// PowerPC is `ppc64le`. Parsing also accepts the common aliases `amd64`,
/// `arm64`, `i386`, `i686` and `powerpc64le`.
///
/// # Examples
///
/// ```
/// use coblet_types::Arch;
///
/// assert_eq!("arm64".parse::<Arch>()?, Arch::Aarch64);
/// assert_eq!(Arch::Aarch64.as_str(), "aarch64");
/// # Ok::<(), coblet_types::PlatformNameError>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Arch {
    /// 32-bit x86.
    X86,
    /// 64-bit x86.
    X86_64,
    /// 64-bit ARM.
    Aarch64,
    /// 32-bit ARM.
    Arm,
    /// 64-bit RISC-V.
    Riscv64,
    /// Little-endian 64-bit PowerPC.
    Ppc64le,
    /// An architecture the engine does not know, kept by name.
    Other(ArchName),
}

impl Arch {
    /// Returns the canonical name, such as `x86_64`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Arm => "arm",
            Self::Riscv64 => "riscv64",
            Self::Ppc64le => "ppc64le",
            Self::Other(name) => name.as_str(),
        }
    }
}

impl From<ArchName> for Arch {
    /// Maps a known name or alias to its variant and keeps the rest as [`Arch::Other`].
    fn from(name: ArchName) -> Self {
        match name.as_str() {
            "x86" | "i386" | "i686" => Self::X86,
            "x86_64" | "amd64" => Self::X86_64,
            "aarch64" | "arm64" => Self::Aarch64,
            "arm" => Self::Arm,
            "riscv64" => Self::Riscv64,
            "ppc64le" | "powerpc64le" => Self::Ppc64le,
            _ => Self::Other(name),
        }
    }
}

impl std::str::FromStr for Arch {
    type Err = PlatformNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ArchName::new(s).map(Self::from)
    }
}

impl TryFrom<String> for Arch {
    type Error = PlatformNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Arch> for String {
    fn from(value: Arch) -> Self {
        value.as_str().to_owned()
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "../../tests/platform/arch.rs"]
mod tests;
