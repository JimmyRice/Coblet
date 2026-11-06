use coblet_types::{Arch, Libc, OsFamily, Platform, PlatformNameError};

/// Detects the platform the process runs on.
///
/// Callers that know better (a sandbox, a remote target, a test) can skip
/// probing and build a [`Platform`] by hand.
pub trait PlatformProbe: Send + Sync + 'static {
    /// Describes the current platform.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError`] when the platform cannot be described.
    fn detect(&self) -> Result<Platform, ProbeError>;
}

/// Reason platform detection failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// A detected name is not a valid platform name.
    #[error("detected platform name is invalid: {0}")]
    InvalidName(#[from] PlatformNameError),
}

/// Probe that reports the compile target from [`std::env::consts`] and `target_env`.
///
/// It reads no files and spawns no processes, so the OS version stays unset.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdPlatformProbe;

impl PlatformProbe for StdPlatformProbe {
    fn detect(&self) -> Result<Platform, ProbeError> {
        let os: OsFamily = std::env::consts::OS.parse()?;
        let arch: Arch = target_arch().parse()?;
        let platform = Platform::new(os, arch);
        Ok(match target_libc() {
            Some(libc) => platform.with_libc(libc),
            None => platform,
        })
    }
}

fn target_arch() -> &'static str {
    // std reports both endiannesses of 64-bit PowerPC as "powerpc64"; only
    // the little-endian one has a variant.
    if std::env::consts::ARCH == "powerpc64" && cfg!(target_endian = "little") {
        "ppc64le"
    } else {
        std::env::consts::ARCH
    }
}

fn target_libc() -> Option<Libc> {
    if cfg!(target_env = "gnu") && !cfg!(windows) {
        Some(Libc::Glibc)
    } else if cfg!(target_env = "musl") {
        Some(Libc::Musl)
    } else {
        None
    }
}

#[cfg(test)]
#[path = "../tests/platform_probe.rs"]
mod tests;
