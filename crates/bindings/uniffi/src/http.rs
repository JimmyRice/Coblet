use std::sync::{Arc, mpsc};

use coblet::runtime::{HttpUrl, Method, Request, Transport};

use crate::{CobletError, ErrorKind};

/// Largest response body [`send`] reads.
const BODY_LIMIT: u64 = 1 << 20;

/// One HTTP header.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HttpHeader {
    /// Name, such as `Content-Type`.
    pub name: String,
    /// Value.
    pub value: String,
}

/// HTTP request sent through coblet's transport, so mirrors, the proxy
/// and timeouts apply as they do to coblet's own requests.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HttpRequest {
    /// `GET`, `HEAD`, `POST`, `PUT` or `DELETE`.
    pub method: String,
    /// Address, `http` or `https`.
    pub url: String,
    /// Headers, sent as given.
    pub headers: Vec<HttpHeader>,
    /// Body, if any.
    pub body: Option<Vec<u8>>,
}

/// Answer to an [`HttpRequest`]; any status, including errors.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HttpResponse {
    /// Status code, such as 200.
    pub status: u16,
    /// Headers.
    pub headers: Vec<HttpHeader>,
    /// Body, at most 1 MiB.
    pub body: Vec<u8>,
}

/// Builds a transport request from a foreign one.
fn build_request(request: HttpRequest) -> Result<Request, CobletError> {
    let invalid = |message: String| CobletError::new(ErrorKind::InvalidArgument, message);
    let method = match request.method.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        other => return Err(invalid(format!("unknown http method `{other}`"))),
    };
    let url = HttpUrl::new(request.url).map_err(|error| invalid(error.to_string()))?;
    let mut built = Request::new(method, url);
    for header in request.headers {
        built = built.header(header.name, header.value);
    }
    if let Some(body) = request.body {
        built = built.body(body);
    }
    Ok(built)
}

/// Sends `request` on `runtime` and waits for the answer; call it from a
/// thread outside the runtime, such as the one a foreign callback runs on.
pub(crate) fn send(
    runtime: &tokio::runtime::Handle,
    transport: &Arc<dyn Transport>,
    request: HttpRequest,
) -> Result<HttpResponse, CobletError> {
    let built = build_request(request)?;
    let transport = Arc::clone(transport);
    let (sender, receiver) = mpsc::sync_channel(1);
    runtime.spawn(async move {
        let answer = async {
            let response = transport.send(built).await?;
            let status = response.status();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| HttpHeader {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect();
            let body = response.bytes(BODY_LIMIT).await?.to_vec();
            Ok::<_, coblet::runtime::TransportError>(HttpResponse {
                status,
                headers,
                body,
            })
        };
        let _ = sender.send(answer.await);
    });
    receiver
        .recv()
        .map_err(|_| CobletError::new(ErrorKind::Other, "the request was dropped"))?
        .map_err(|error| CobletError::new(ErrorKind::Network, error.to_string()))
}
