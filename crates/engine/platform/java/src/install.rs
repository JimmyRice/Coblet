use std::fmt;
use std::path::PathBuf;

use coblet_types::{Arch, ExtensionId};

/// Where an installation was found.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InstallSource {
    /// The `JAVA_HOME` environment variable.
    JavaHome,
    /// A directory on `PATH`.
    Path,
    /// An SDKMAN! candidate directory.
    Sdkman,
    /// A system directory such as `/Library/Java/JavaVirtualMachines` or `/usr/lib/jvm`.
    SystemDirectory,
    /// A Mojang runtime coblet installed before Java providers, named
    /// after its component folder.
    MojangRuntime {
        /// Runtime component, such as `jre-legacy`.
        component: String,
    },
    /// A path given by the application or the user.
    Configured,
    /// A runtime a Java provider installed.
    Provider {
        /// The provider.
        provider: ExtensionId,
        /// The package ID.
        package: String,
    },
}

/// Java installation whose version was probed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JavaInstall {
    /// The `java` executable.
    pub executable: PathBuf,
    /// `java.home` as reported by the runtime.
    pub home: PathBuf,
    /// `java.version`, such as `1.8.0_74` or `17.0.21`.
    pub version: String,
    /// Major version, such as 8 or 17.
    pub major: u32,
    /// Architecture the runtime was built for (`os.arch`).
    pub arch: Arch,
    /// `java.vendor`, when reported.
    pub vendor: Option<String>,
    /// Where it was found.
    pub source: InstallSource,
}

impl fmt::Display for JavaInstall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "java {} ({}) at {}",
            self.version,
            self.arch.as_str(),
            self.executable.display()
        )
    }
}

/// Returns the major version of a `java.version` string: `1.8.0_74` is 8, `17.0.21` is 17.
#[must_use]
pub fn major_version(version: &str) -> Option<u32> {
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::major_version;

    #[test]
    fn reads_major_versions() {
        assert_eq!(major_version("1.8.0_74"), Some(8));
        assert_eq!(major_version("17.0.21"), Some(17));
        assert_eq!(major_version("25"), Some(25));
        assert_eq!(major_version("21-ea"), Some(21));
        assert_eq!(major_version(""), None);
    }
}
