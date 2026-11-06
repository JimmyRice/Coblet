use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rustls_platform_verifier::BuilderVerifierExt;

use super::{Body, Method, Request, Response, Transport, TransportError, UserAgent};
use crate::BoxFuture;

/// Time allowed for the TCP and TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest pause between two reads of a response before the request fails.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Default [`Transport`]: reqwest with rustls, HTTP/2 and the operating system's trust store.
///
/// TLS uses the `ring` crypto provider and verifies certificates through
/// `rustls-platform-verifier`, so enterprise roots installed in the system
/// store are honored. Every request carries the caller's [`UserAgent`].
///
/// # Examples
///
/// ```no_run
/// use coblet_runtime::{HttpUrl, ReqwestTransport, Request, Transport, UserAgent};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let transport = ReqwestTransport::new(&UserAgent::new("MyLauncher/1.0")?)?;
/// let url = HttpUrl::new("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")?;
/// let manifest = transport.send(Request::get(url)).await?.bytes(16 << 20).await?;
/// # drop(manifest);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Builds a client.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Tls`] when the platform certificate
    /// verifier cannot be set up, or [`TransportError::InvalidRequest`] when
    /// the client cannot be built.
    pub fn new(user_agent: &UserAgent) -> Result<Self, TransportError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| TransportError::Tls(Box::new(e)))?
            .with_platform_verifier()
            .map_err(|e| TransportError::Tls(Box::new(e)))?
            .with_no_client_auth();
        // Offer HTTP/2 so thousands of small asset downloads share connections.
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let client = reqwest::Client::builder()
            .user_agent(user_agent.as_str())
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|e| TransportError::InvalidRequest(e.to_string()))?;
        Ok(Self { client })
    }
}

fn map_error(error: reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout
    } else if error.is_builder() {
        TransportError::InvalidRequest(error.to_string())
    } else if error.is_body() || error.is_decode() {
        TransportError::Interrupted(Box::new(error))
    } else {
        TransportError::Connect(Box::new(error))
    }
}

struct ReqwestBody(reqwest::Response);

impl Body for ReqwestBody {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, TransportError>> {
        Box::pin(async move {
            self.0.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::Interrupted(Box::new(error))
                }
            })
        })
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async move {
            let method = match request.method() {
                Method::Get => reqwest::Method::GET,
                Method::Head => reqwest::Method::HEAD,
                Method::Post => reqwest::Method::POST,
                Method::Put => reqwest::Method::PUT,
                Method::Delete => reqwest::Method::DELETE,
            };
            let mut builder = self.client.request(method, request.url().as_str());
            for (name, value) in request.headers() {
                builder = builder.header(name.as_str(), value.as_str());
            }
            if let Some(offset) = request.resume_offset() {
                builder = builder.header("Range", format!("bytes={offset}-"));
            }
            if let Some(body) = request.body_bytes() {
                builder = builder.body(body.clone());
            }
            let response = builder.send().await.map_err(map_error)?;
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect();
            Ok(Response::new(
                response.status().as_u16(),
                headers,
                ReqwestBody(response),
            ))
        })
    }
}

#[cfg(test)]
#[path = "../../tests/transport/reqwest_client.rs"]
mod tests;
