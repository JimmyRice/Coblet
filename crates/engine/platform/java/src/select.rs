use coblet_platform::JavaRequirement;

use crate::JavaInstall;

/// Picks the installation for a requirement, in discovery order: one with
/// the required major version and architecture, or, when the requirement's
/// [`JavaMatch`](coblet_platform::JavaMatch) allows newer versions, the
/// oldest accepted newer one.
#[must_use]
pub fn select<'a>(
    installs: &'a [JavaInstall],
    requirement: &JavaRequirement,
) -> Option<&'a JavaInstall> {
    let accepted = || {
        installs
            .iter()
            .filter(|install| requirement.accepts(install.major, &install.arch))
    };
    accepted()
        .find(|install| install.major == requirement.major)
        .or_else(|| accepted().min_by_key(|install| install.major))
}
