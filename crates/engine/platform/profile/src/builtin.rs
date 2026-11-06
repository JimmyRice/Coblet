use std::sync::{Arc, OnceLock};

use crate::{DataProfile, PlatformProfile, ProfileLayer};

/// Built-in profile files, embedded at build time.
const FILES: [(&str, &str); 15] = [
    (
        "mojang-windows-x64.toml",
        include_str!("../data/profiles/mojang-windows-x64.toml"),
    ),
    (
        "mojang-windows-x86.toml",
        include_str!("../data/profiles/mojang-windows-x86.toml"),
    ),
    (
        "mojang-windows-arm64.toml",
        include_str!("../data/profiles/mojang-windows-arm64.toml"),
    ),
    (
        "windows-arm64-emulation.toml",
        include_str!("../data/profiles/windows-arm64-emulation.toml"),
    ),
    (
        "mojang-macos-x64.toml",
        include_str!("../data/profiles/mojang-macos-x64.toml"),
    ),
    (
        "mojang-macos-arm64.toml",
        include_str!("../data/profiles/mojang-macos-arm64.toml"),
    ),
    (
        "macos-arm64-rosetta.toml",
        include_str!("../data/profiles/macos-arm64-rosetta.toml"),
    ),
    (
        "macos-lwjgl3.toml",
        include_str!("../data/profiles/macos-lwjgl3.toml"),
    ),
    (
        "mojang-linux-x64.toml",
        include_str!("../data/profiles/mojang-linux-x64.toml"),
    ),
    (
        "mojang-linux-x86.toml",
        include_str!("../data/profiles/mojang-linux-x86.toml"),
    ),
    (
        "linux-arm64.toml",
        include_str!("../data/profiles/linux-arm64.toml"),
    ),
    (
        "linux-riscv64.toml",
        include_str!("../data/profiles/linux-riscv64.toml"),
    ),
    (
        "freebsd-system.toml",
        include_str!("../data/profiles/freebsd-system.toml"),
    ),
    (
        "openbsd-system.toml",
        include_str!("../data/profiles/openbsd-system.toml"),
    ),
    (
        "legacy-arguments.toml",
        include_str!("../data/profiles/legacy-arguments.toml"),
    ),
];

static PARSED: OnceLock<Vec<Arc<dyn PlatformProfile>>> = OnceLock::new();

/// Returns the built-in profiles, parsed on first use.
///
/// A file that fails to parse is skipped and logged; a unit test keeps all of them valid.
pub(crate) fn profiles() -> Vec<Arc<dyn PlatformProfile>> {
    PARSED
        .get_or_init(|| {
            FILES
                .iter()
                .filter_map(|(name, text)| {
                    match DataProfile::from_toml(text, ProfileLayer::Builtin) {
                        Ok(profile) => Some(Arc::new(profile) as Arc<dyn PlatformProfile>),
                        Err(error) => {
                            tracing::error!(file = name, %error, "skipping invalid built-in profile");
                            None
                        }
                    }
                })
                .collect()
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::FILES;
    use crate::{DataProfile, ProfileLayer};

    #[test]
    fn every_builtin_profile_parses() {
        for (name, text) in FILES {
            DataProfile::from_toml(text, ProfileLayer::Builtin)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        assert_eq!(super::profiles().len(), FILES.len());
    }
}
