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
