use super::{Request, Response, TransportError};
use crate::BoxFuture;

/// HTTP client used by every crate that talks to the network.
///
/// The default implementation (reqwest with rustls) is added together with
/// the downloader. Tests use in-memory implementations, so the default test
/// suite never touches the network.
pub trait Transport: Send + Sync + 'static {
    /// Sends a request and returns once the response headers have arrived.
    ///
    /// Redirects are followed by the implementation. A non-2xx status is a
    /// normal [`Response`], not an error.
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>>;
}

#[cfg(test)]
#[path = "../../tests/transport/client.rs"]
mod tests;
