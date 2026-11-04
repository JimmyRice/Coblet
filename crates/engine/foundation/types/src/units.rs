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
mod tests {
    use super::MiB;

    #[test]
    fn converts_to_bytes_without_overflow() {
        assert_eq!(MiB(0).bytes(), 0);
        assert_eq!(MiB(1).bytes(), 1_048_576);
        assert_eq!(MiB(u32::MAX).bytes(), u64::from(u32::MAX) * 1_048_576);
    }

    #[test]
    fn orders_by_amount() {
        assert!(MiB(512) < MiB(2048));
        assert_eq!(serde_json::to_string(&MiB(2048)).unwrap(), "2048");
    }
}
