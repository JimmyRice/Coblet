use std::time::{Duration, SystemTime};

use super::{backoff, is_retryable_status, retry_after};

#[test]
fn classifies_statuses() {
    assert!(is_retryable_status(503));
    assert!(is_retryable_status(429));
    assert!(!is_retryable_status(404));
    assert!(!is_retryable_status(403));
}

#[test]
fn backs_off_exponentially_with_bounded_jitter() {
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(1);
    for attempt in 0..3 {
        let wait = backoff(attempt, base, max);
        let floor = base * 2_u32.pow(attempt);
        assert!(wait >= floor && wait <= floor * 3 / 2, "{wait:?}");
    }
    assert_eq!(backoff(10, base, max), max);
}

#[test]
fn parses_retry_after() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(784_111_777);
    assert_eq!(retry_after(" 120 ", now), Some(Duration::from_secs(120)));
    assert_eq!(
        retry_after("Sun, 06 Nov 1994 08:49:47 GMT", now),
        Some(Duration::from_secs(10))
    );
    assert_eq!(
        retry_after(
            "Sun, 06 Nov 1994 08:49:37 GMT",
            now + Duration::from_secs(5)
        ),
        Some(Duration::ZERO)
    );
    assert_eq!(retry_after("soon", now), None);
}
