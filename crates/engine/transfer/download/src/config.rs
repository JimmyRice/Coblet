use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

/// Limits and retry policy of a [`Downloader`](crate::Downloader).
///
/// # Examples
///
/// ```
/// use std::num::NonZeroUsize;
///
/// use coblet_download::DownloaderConfig;
///
/// let config = DownloaderConfig::default()
///     .with_limits(NonZeroUsize::new(32).unwrap(), NonZeroUsize::new(8).unwrap())
///     .with_cache_root("/data/cache");
/// assert_eq!(config.max_concurrent.get(), 32);
/// assert_eq!(config.attempts_per_source, 3);
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloaderConfig {
    /// Requests in flight across all hosts.
    pub max_concurrent: NonZeroUsize,
    /// Requests in flight to one host.
    pub max_per_host: NonZeroUsize,
    /// Attempts at one address before moving to the next, for retryable failures.
    pub attempts_per_source: u32,
    /// Wait before the first retry; doubles after each attempt, plus up to 50 % jitter.
    pub backoff_base: Duration,
    /// Longest wait between attempts, including a server's `Retry-After`.
    pub backoff_max: Duration,
    /// Root of the content-addressed cache (`<root>/objects/<algorithm>/<hash>`),
    /// usually `data_root/cache`.
    pub cache_root: Option<PathBuf>,
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        Self {
            max_concurrent: NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
            max_per_host: NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            attempts_per_source: 3,
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            cache_root: None,
        }
    }
}

impl DownloaderConfig {
    /// Sets the global and per-host request limits.
    #[must_use]
    pub fn with_limits(self, max_concurrent: NonZeroUsize, max_per_host: NonZeroUsize) -> Self {
        Self {
            max_concurrent,
            max_per_host,
            ..self
        }
    }

    /// Sets the retry policy.
    #[must_use]
    pub fn with_retry(self, attempts_per_source: u32, base: Duration, max: Duration) -> Self {
        Self {
            attempts_per_source,
            backoff_base: base,
            backoff_max: max,
            ..self
        }
    }

    /// Sets the root of the content-addressed cache.
    #[must_use]
    pub fn with_cache_root(self, root: impl Into<PathBuf>) -> Self {
        Self {
            cache_root: Some(root.into()),
            ..self
        }
    }
}
