use std::fmt;

use serde::{Deserialize, Serialize};

/// Amount of memory in mebibytes (1 MiB = 1 048 576 bytes), the unit of JVM heap settings.
///
/// # Examples
///
/// ```
/// use coblet_types::MiB;
///
/// let heap = MiB(4096);
/// assert_eq!(heap.to_string(), "4096 MiB");
/// assert_eq!(heap.bytes(), 4_294_967_296);
/// ```
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MiB(pub u32);

impl MiB {
    /// Returns the amount in bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        // Widening u32 to u64 is lossless and u32::MAX * 2^20 fits in u64.
        (self.0 as u64) << 20
    }
}

impl fmt::Display for MiB {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} MiB", self.0)
    }
}

#[cfg(test)]
#[path = "../tests/units.rs"]
mod tests;
