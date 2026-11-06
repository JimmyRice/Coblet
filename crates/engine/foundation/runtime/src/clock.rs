use std::time::SystemTime;

/// Source of the current wall-clock time.
///
/// Token expiry checks and journal timestamps read time through this trait
/// so tests can pin it.
pub trait Clock: Send + Sync + 'static {
    /// Returns the current time.
    fn now(&self) -> SystemTime;
}

/// Clock backed by [`SystemTime::now`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Clock that always returns the same instant.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, SystemTime};
///
/// use coblet_runtime::{Clock, FixedClock};
///
/// let noon = SystemTime::UNIX_EPOCH + Duration::from_secs(43_200);
/// assert_eq!(FixedClock(noon).now(), noon);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedClock(pub SystemTime);

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{Clock, FixedClock, SystemClock};

    #[test]
    fn system_clock_moves_forward() {
        let clock: &dyn Clock = &SystemClock;
        let first = clock.now();
        assert!(clock.now() >= first);
        assert!(first > SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    }

    #[test]
    fn fixed_clock_does_not_move() {
        let clock = FixedClock(SystemTime::UNIX_EPOCH);
        assert_eq!(clock.now(), clock.now());
    }
}
