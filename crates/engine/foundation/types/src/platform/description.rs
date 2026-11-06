use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{Arch, Libc, OsFamily, PlatformTrait};

/// Description of the machine a game runs on, the input to Platform Profile matching.
///
/// Callers can detect it through a `PlatformProbe` or build it by hand, which
/// is how tests describe platforms the build host is not.
///
/// # Examples
///
/// ```
/// use coblet_types::{Arch, OsFamily, Platform, PlatformTrait};
///
/// let freebsd = Platform::new(OsFamily::FreeBsd, Arch::X86_64)
///     .with_os_version("14.2")
///     .with_trait(PlatformTrait::new("wayland")?);
/// assert_eq!(freebsd.os_version(), Some("14.2"));
/// assert!(freebsd.has_trait("wayland"));
/// # Ok::<(), coblet_types::PlatformNameError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    os: OsFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    os_version: Option<String>,
    arch: Arch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    libc: Option<Libc>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    traits: BTreeSet<PlatformTrait>,
}

impl Platform {
    /// Describes a platform by operating system and architecture only.
    #[must_use]
    pub fn new(os: OsFamily, arch: Arch) -> Self {
        Self {
            os,
            os_version: None,
            arch,
            libc: None,
            traits: BTreeSet::new(),
        }
    }

    /// Returns a copy with the operating system version, such as `14.2` or `10.0.22631`.
    #[must_use]
    pub fn with_os_version(self, version: impl Into<String>) -> Self {
        Self {
            os_version: Some(version.into()),
            ..self
        }
    }

    /// Returns a copy with the C library.
    #[must_use]
    pub fn with_libc(self, libc: Libc) -> Self {
        Self {
            libc: Some(libc),
            ..self
        }
    }

    /// Returns a copy with one more trait.
    #[must_use]
    pub fn with_trait(mut self, platform_trait: PlatformTrait) -> Self {
        self.traits.insert(platform_trait);
        self
    }

    /// Returns the operating system family.
    #[must_use]
    pub fn os(&self) -> &OsFamily {
        &self.os
    }

    /// Returns the operating system version, if known.
    #[must_use]
    pub fn os_version(&self) -> Option<&str> {
        self.os_version.as_deref()
    }

    /// Returns the CPU architecture.
    #[must_use]
    pub fn arch(&self) -> &Arch {
        &self.arch
    }

    /// Returns the C library, if known.
    #[must_use]
    pub fn libc(&self) -> Option<&Libc> {
        self.libc.as_ref()
    }

    /// Returns all traits in sorted order.
    #[must_use]
    pub fn traits(&self) -> &BTreeSet<PlatformTrait> {
        &self.traits
    }

    /// Reports whether the platform carries the named trait; the name is compared case-insensitively.
    #[must_use]
    pub fn has_trait(&self, name: &str) -> bool {
        PlatformTrait::new(name).is_ok_and(|wanted| self.traits.contains(&wanted))
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;
    use crate::{Arch, Libc, OsFamily, PlatformTrait};

    #[test]
    fn serializes_only_known_fields() {
        let linux = Platform::new(OsFamily::Linux, Arch::Riscv64).with_libc(Libc::Musl);
        let json = serde_json::to_string(&linux).unwrap();
        assert_eq!(json, r#"{"os":"linux","arch":"riscv64","libc":"musl"}"#);
        assert_eq!(serde_json::from_str::<Platform>(&json).unwrap(), linux);
    }

    #[test]
    fn describes_unknown_platforms() {
        let json = r#"{"os":"haiku","arch":"sparc64","traits":["Wayland"]}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(platform.os().as_str(), "haiku");
        assert_eq!(platform.arch().as_str(), "sparc64");
        assert!(platform.has_trait("WAYLAND"));
        assert!(!platform.has_trait("rosetta"));
        assert!(!platform.has_trait("not a trait"));
        assert_eq!(
            platform.traits().iter().collect::<Vec<_>>(),
            [&PlatformTrait::new("wayland").unwrap()]
        );
    }
}
