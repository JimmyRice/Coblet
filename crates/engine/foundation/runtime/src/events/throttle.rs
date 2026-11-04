use std::time::{Duration, Instant};

/// Default spacing between progress events of one Operation (30 per second).
pub const DEFAULT_PROGRESS_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Rate limiter for progress events, so FFI callbacks are not flooded.
///
/// The first update always passes, and so does a final update (`done == total`),
/// so subscribers always see completion.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
///
/// use coblet_runtime::ProgressThrottle;
///
/// let mut throttle = ProgressThrottle::new(Duration::from_millis(100));
/// let start = Instant::now();
/// assert!(throttle.allow(start, 10, Some(100)));
/// assert!(!throttle.allow(start + Duration::from_millis(50), 20, Some(100)));
/// assert!(throttle.allow(start + Duration::from_millis(60), 100, Some(100)));
/// ```
#[derive(Clone, Debug)]
pub struct ProgressThrottle {
    interval: Duration,
    last: Option<Instant>,
}

impl ProgressThrottle {
    /// Creates a throttle that lets one update through per `interval`.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// Reports whether an update at `now` should be published, and records it if so.
    pub fn allow(&mut self, now: Instant, done: u64, total: Option<u64>) -> bool {
        let complete = total == Some(done);
        let due = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval);
        if complete || due {
            self.last = Some(now);
        }
        complete || due
    }
}

impl Default for ProgressThrottle {
    fn default() -> Self {
        Self::new(DEFAULT_PROGRESS_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{DEFAULT_PROGRESS_INTERVAL, ProgressThrottle};

    #[test]
    fn caps_updates_per_second() {
        let mut throttle = ProgressThrottle::default();
        let start = Instant::now();
        let passed = (0..1000_u64)
            .filter(|&ms| throttle.allow(start + Duration::from_millis(ms), ms, None))
            .count();
        assert!(
            (29..=31).contains(&passed),
            "{passed} updates passed in one second"
        );
        assert!(DEFAULT_PROGRESS_INTERVAL < Duration::from_millis(34));
    }

    #[test]
    fn always_passes_completion() {
        let mut throttle = ProgressThrottle::new(Duration::from_secs(60));
        let now = Instant::now();
        assert!(throttle.allow(now, 0, Some(5)));
        assert!(!throttle.allow(now, 3, Some(5)));
        assert!(throttle.allow(now, 5, Some(5)));
    }
}
