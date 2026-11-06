//! Retry policy: which failures to repeat and how long to wait.

use std::hash::{BuildHasher, RandomState};
use std::time::{Duration, SystemTime};

/// Reports whether an HTTP status is worth repeating the request for.
///
/// 408, 425 and 429 ask the client to try later; 500, 502, 503 and 504 are
/// usually transient server or gateway failures.
pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

/// Exponential backoff with up to 50 % random jitter, capped at `max`.
pub(crate) fn backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    let exponential = base.saturating_mul(2_u32.saturating_pow(attempt));
    // A fresh RandomState is seeded randomly, which is enough to spread
    // retries from many tasks without a random number crate.
    let random = RandomState::new().hash_one(attempt);
    let jitter_permille = u32::try_from(random % 501).unwrap_or(0);
    let jitter = exponential.saturating_mul(jitter_permille) / 1000;
    exponential.saturating_add(jitter).min(max)
}

/// Parses a `Retry-After` header: delay seconds or an HTTP date.
///
/// See <https://www.rfc-editor.org/rfc/rfc9110#field.retry-after>.
pub(crate) fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
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
}
