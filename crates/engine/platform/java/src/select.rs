use coblet_platform::JavaRequirement;

use crate::JavaInstall;

/// Picks the installation for a requirement: same major version and
/// architecture, in discovery order.
///
/// The major version must match exactly: launchwrapper versions break on
/// Java 9 and later, and newer game versions are tested on one Java line.
#[must_use]
pub fn select<'a>(
    installs: &'a [JavaInstall],
    requirement: &JavaRequirement,
) -> Option<&'a JavaInstall> {
    installs
        .iter()
        .find(|install| install.major == requirement.major && install.arch == requirement.arch)
}
