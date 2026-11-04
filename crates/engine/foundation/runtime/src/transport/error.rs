use std::error::Error;

/// Failure to complete an HTTP exchange.
///
/// HTTP status codes are not errors at this level; callers inspect
/// [`Response::status`](crate::Response::status). [`TransportError::is_retryable`]
/// tells the downloader whether another attempt can succeed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The request is malformed, such as an invalid header name.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// No connection could be made (DNS, refused, reset before the response).
    #[error("connection failed: {0}")]
    Connect(#[source] Box<dyn Error + Send + Sync>),
    /// The exchange took longer than the configured timeout.
    #[error("request timed out")]
    Timeout,
    /// The response body stopped before it was complete.
    #[error("response body was interrupted: {0}")]
    Interrupted(#[source] Box<dyn Error + Send + Sync>),
    /// The TLS handshake or certificate check failed.
    #[error("tls failed: {0}")]
    Tls(#[source] Box<dyn Error + Send + Sync>),
    /// The body is larger than the limit passed to [`Response::bytes`](crate::Response::bytes).
    #[error("response body exceeds the {limit} byte limit")]
    BodyTooLarge {
        /// The limit in bytes.
        limit: u64,
    },
}

impl TransportError {
    /// Reports whether repeating the request can succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Connect(_) | Self::Timeout | Self::Interrupted(_)
        )
    }
}
