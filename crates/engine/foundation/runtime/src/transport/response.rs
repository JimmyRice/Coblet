use bytes::{Bytes, BytesMut};

use super::TransportError;
use crate::BoxFuture;

/// Response body read chunk by chunk, so large files never sit in memory whole.
pub trait Body: Send {
    /// Returns the next chunk, or `None` once the body is complete.
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, TransportError>>;
}

/// Body served from memory, for mock transports and small responses.
#[derive(Clone, Debug, Default)]
pub struct MemoryBody(Option<Bytes>);

impl MemoryBody {
    /// Wraps a complete body.
    #[must_use]
    pub fn new(bytes: impl Into<Bytes>) -> Self {
        Self(Some(bytes.into()))
    }
}

impl Body for MemoryBody {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, TransportError>> {
        let chunk = self.0.take().filter(|bytes| !bytes.is_empty());
        Box::pin(async move { Ok(chunk) })
    }
}

/// HTTP response with a streaming body.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{MemoryBody, Response};
///
/// # tokio_test_block_on(async {
/// let response = Response::new(200, vec![("ETag".into(), "\"v1\"".into())], MemoryBody::new("hi"));
/// assert_eq!(response.header("etag"), Some("\"v1\""));
/// assert_eq!(&response.bytes(1024).await?[..], b"hi");
/// # Ok::<(), coblet_runtime::TransportError>(())
/// # });
/// # fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
/// #     tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
/// # }
/// ```
pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Box<dyn Body>,
}

impl Response {
    /// Assembles a response; transports and mocks call this.
    #[must_use]
    pub fn new(status: u16, headers: Vec<(String, String)>, body: impl Body + 'static) -> Self {
        Self {
            status,
            headers,
            body: Box::new(body),
        }
    }

    /// Returns the status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Reports whether the status is 2xx.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Returns the first value of a header, matching the name case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Returns all headers in received order.
    #[must_use]
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Returns the next body chunk, or `None` at the end.
    ///
    /// # Errors
    ///
    /// Returns the [`TransportError`] reported by the body.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        self.body.next_chunk().await
    }

    /// Reads the whole body into memory.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::BodyTooLarge`] once more than `limit` bytes
    /// arrive, or the error reported by the body.
    pub async fn bytes(mut self, limit: u64) -> Result<Bytes, TransportError> {
        let mut collected = BytesMut::new();
        while let Some(chunk) = self.body.next_chunk().await? {
            let total = u64::try_from(collected.len() + chunk.len()).unwrap_or(u64::MAX);
            if total > limit {
                return Err(TransportError::BodyTooLarge { limit });
            }
            collected.extend_from_slice(&chunk);
        }
        Ok(collected.freeze())
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../../tests/transport/response.rs"]
mod tests;
