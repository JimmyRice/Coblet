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
#[path = "../tests/retry.rs"]
mod tests;
