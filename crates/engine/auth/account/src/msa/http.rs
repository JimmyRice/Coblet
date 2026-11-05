use std::fmt::Write as _;

use coblet_runtime::{HttpUrl, Method, Request};
use serde::de::DeserializeOwned;

use crate::{AuthCtx, AuthError};

/// Largest response body read from an auth endpoint.
const BODY_LIMIT: u64 = 1 << 20;

/// Longest service message kept in an error.
const MESSAGE_LIMIT: usize = 300;

/// Status and body of a finished exchange.
pub(crate) struct Reply {
    pub(crate) endpoint: &'static str,
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

impl Reply {
    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parses the body as JSON.
    pub(crate) fn json<T: DeserializeOwned>(&self) -> Result<T, AuthError> {
        serde_json::from_slice(&self.body).map_err(|error| AuthError::InvalidResponse {
            endpoint: self.endpoint,
            message: error.to_string(),
        })
    }

    /// Returns the body as text, shortened for error messages.
    pub(crate) fn message(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        let text = text.trim();
        match text.char_indices().nth(MESSAGE_LIMIT) {
            Some((cut, _)) => format!("{}...", &text[..cut]),
            None => text.to_owned(),
        }
    }

    /// Maps an unexpected status to an error.
    pub(crate) fn unexpected(&self) -> AuthError {
        if self.status == 429 {
            return AuthError::RateLimited {
                endpoint: self.endpoint,
            };
        }
        AuthError::Http {
            endpoint: self.endpoint,
            status: self.status,
            message: self.message(),
        }
    }
}

/// Sends a request and reads the whole body.
pub(crate) async fn send(
    ctx: &AuthCtx,
    endpoint: &'static str,
    request: Request,
) -> Result<Reply, AuthError> {
    let transport = |source| AuthError::Transport { endpoint, source };
    let response = ctx.transport().send(request).await.map_err(transport)?;
    let status = response.status();
    let body = response.bytes(BODY_LIMIT).await.map_err(transport)?;
    Ok(Reply {
        endpoint,
        status,
        body: body.to_vec(),
    })
}

/// Builds a `POST` with an `application/x-www-form-urlencoded` body.
pub(crate) fn form_post(url: &str, pairs: &[(&str, &str)]) -> Result<Request, AuthError> {
    Ok(Request::new(Method::Post, parse_url(url)?)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(encode_pairs(pairs)))
}

/// Builds a `POST` with a JSON body.
pub(crate) fn json_post(url: &str, body: &serde_json::Value) -> Result<Request, AuthError> {
    Ok(Request::new(Method::Post, parse_url(url)?)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string()))
}

/// Builds a `GET` with a bearer token.
pub(crate) fn bearer_get(url: &str, token: &str) -> Result<Request, AuthError> {
    Ok(Request::get(parse_url(url)?)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json"))
}

pub(crate) fn parse_url(url: &str) -> Result<HttpUrl, AuthError> {
    HttpUrl::new(url).map_err(|error| AuthError::InvalidOptions(error.to_string()))
}

/// Encodes `key=value` pairs joined by `&`.
pub(crate) fn encode_pairs(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(key));
        out.push('=');
        out.push_str(&percent_encode(value));
    }
    out
}

/// Percent-encodes everything except the unreserved characters of RFC 3986.
pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Decodes `%XX` escapes and `+` as a space; `None` for a broken escape or
/// a result that is not UTF-8.
pub(crate) fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Returns the decoded query parameters of a URI, ignoring the fragment.
pub(crate) fn query_pairs(uri: &str) -> Option<Vec<(String, String)>> {
    let without_fragment = uri.split('#').next().unwrap_or_default();
    let Some((_, query)) = without_fragment.split_once('?') else {
        return Some(Vec::new());
    };
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(key)?, percent_decode(value)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{encode_pairs, percent_decode, percent_encode, query_pairs};

    #[test]
    fn encodes_and_decodes() {
        assert_eq!(percent_encode("a b/c~"), "a%20b%2Fc~");
        assert_eq!(
            encode_pairs(&[("scope", "XboxLive.signin offline_access"), ("x", "1")]),
            "scope=XboxLive.signin%20offline_access&x=1"
        );
        assert_eq!(percent_decode("a+b%2Fc").as_deref(), Some("a b/c"));
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%4"), None);
    }

    #[test]
    fn parses_queries() {
        let pairs = query_pairs("http://127.0.0.1:5/cb?code=M.a%2Bb&state=s1#frag").unwrap();
        assert_eq!(
            pairs,
            vec![
                ("code".to_owned(), "M.a+b".to_owned()),
                ("state".to_owned(), "s1".to_owned())
            ]
        );
        assert_eq!(query_pairs("app://cb").unwrap(), Vec::new());
        assert!(query_pairs("app://cb?x=%").is_none());
    }
}
