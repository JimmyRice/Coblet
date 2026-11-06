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
