use std::sync::Arc;

use coblet::runtime::{HttpUrl, Method, Request, Transport};
use serde::Deserialize;

use crate::guard::put;
use crate::types::{CobletBytes, CobletErrorKind, CobletStatus, CobletString, Failure};

/// Largest response body [`send`] reads.
const BODY_LIMIT: u64 = 1 << 20;

/// Request handed over as JSON: `{"method","url","headers":[[name,value]]}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestJson {
    method: String,
    url: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
}

fn build_request(json: &str, body: Option<Vec<u8>>) -> Result<Request, Failure> {
    let request: RequestJson = serde_json::from_str(json)
        .map_err(|error| Failure::argument("request_json", &error.to_string()))?;
    let method = match request.method.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        other => {
            return Err(Failure::argument(
                "request_json",
                &format!("has unknown method `{other}`"),
            ));
        }
    };
    let url = HttpUrl::new(request.url)
        .map_err(|error| Failure::argument("request_json", &error.to_string()))?;
    let mut built = Request::new(method, url);
    for (name, value) in request.headers {
        built = built.header(name, value);
    }
    if let Some(body) = body {
        built = built.body(body);
    }
    Ok(built)
}

/// Sends a request described by `json` and returns the response as
/// `{"schema":1,"status","headers":[[name,value]]}` with its body; call it
/// from a callback thread outside the runtime.
pub(crate) fn send(
    runtime: &tokio::runtime::Handle,
    transport: &Arc<dyn Transport>,
    json: &str,
    body: Option<Vec<u8>>,
) -> Result<(String, Vec<u8>), Failure> {
    let request = build_request(json, body)?;
    runtime
        .block_on(async {
            let response = transport.send(request).await?;
            let head = serde_json::json!({
                "schema": 1,
                "status": response.status(),
                "headers": response.headers(),
            });
            let body = response.bytes(BODY_LIMIT).await?.to_vec();
            Ok::<_, coblet::runtime::TransportError>((head.to_string(), body))
        })
        .map_err(|error| Failure::new(CobletErrorKind::Network, error.to_string()))
}

/// Copies an optional request body argument.
///
/// # Safety
///
/// `body` must be NULL or readable for `len` bytes.
#[allow(unsafe_code, reason = "reads a byte argument")]
pub(crate) unsafe fn body_arg(body: *const u8, len: usize) -> Option<Vec<u8>> {
    if body.is_null() {
        return None;
    }
    // SAFETY: forwarded from the caller's guarantee.
    Some(unsafe { std::slice::from_raw_parts(body, len) }.to_vec())
}

/// Writes a response to the out-parameters of a `*_send` function.
///
/// # Safety
///
/// Both pointers must be NULL or writable.
#[allow(unsafe_code, reason = "writes out-parameters")]
pub(crate) unsafe fn answer(
    (head, body): (String, Vec<u8>),
    out_response_json: *mut CobletString,
    out_body: *mut CobletBytes,
) -> Result<CobletStatus, Failure> {
    if out_response_json.is_null() || out_body.is_null() {
        return Err(Failure::argument(
            "out_response_json or out_body",
            "is NULL",
        ));
    }
    // SAFETY: forwarded from the caller's guarantee.
    unsafe {
        put(
            out_response_json,
            CobletString::new(&head),
            "out_response_json",
        )?;
        put(out_body, CobletBytes::new(body), "out_body")?;
    }
    Ok(CobletStatus::Ok)
}

#[cfg(test)]
mod tests {
    use super::build_request;

    #[test]
    fn requests_are_read_from_json() {
        let request = build_request(
            r#"{"method":"post","url":"https://acme.test/login","headers":[["Accept","application/json"]]}"#,
            Some(b"{}".to_vec()),
        )
        .unwrap();
        assert_eq!(request.url().as_str(), "https://acme.test/login");
        assert!(build_request(r#"{"method":"TRACE","url":"https://acme.test"}"#, None).is_err());
        assert!(build_request(r#"{"method":"GET","url":"ftp://acme.test"}"#, None).is_err());
    }
}
