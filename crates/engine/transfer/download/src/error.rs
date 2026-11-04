use coblet_runtime::{HttpUrl, StorageError, TransportError, VerifyError};

/// Failure of one attempt to fetch one address.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The request did not complete.
    #[error("request to {url} failed: {source}")]
    Transport {
        /// The address.
        url: HttpUrl,
        /// The transport failure.
        #[source]
        source: TransportError,
    },
    /// The server answered with a status other than 200 or 206.
    #[error("{url} answered with status {status}")]
    Status {
        /// The address.
        url: HttpUrl,
        /// The status code.
        status: u16,
    },
    /// The received file does not match the expected size or hashes.
    #[error("file from {url} does not match: {source}")]
    Verify {
        /// The address.
        url: HttpUrl,
        /// The mismatch.
        #[source]
        source: VerifyError,
    },
}

impl FetchError {
    /// Reports whether another attempt at the same address can succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { source, .. } => source.is_retryable(),
            Self::Status { status, .. } => crate::retry::is_retryable_status(*status),
            // A corrupted transfer is repeated once per address, see the downloader.
            Self::Verify { .. } => true,
        }
    }
}

/// Failure of a download task.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// The download was cancelled.
    #[error("download was cancelled")]
    Cancelled,
    /// Every address failed; the error of the last attempt is attached.
    #[error("all {sources} source(s) failed, last error: {last}")]
    AllSourcesFailed {
        /// How many addresses were tried.
        sources: usize,
        /// The last failure.
        #[source]
        last: Box<FetchError>,
    },
    /// Reading or writing a local file failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A content-addressed task has no SHA-1, SHA-256 or SHA-512 hash to key the cache with.
    #[error("content-addressed downloads need a sha1, sha256 or sha512 hash")]
    NoCacheKey,
    /// A content-addressed task was given but the downloader has no cache root.
    #[error("the downloader has no cache root for content-addressed downloads")]
    NoCacheRoot,
}
