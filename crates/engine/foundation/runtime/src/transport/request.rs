use std::fmt;
use std::str::FromStr;

use bytes::Bytes;

/// Absolute `http://` or `https://` URL.
///
/// Only the scheme, a non-empty host part and the absence of whitespace and
/// control characters are checked; the [`Transport`](crate::Transport)
/// implementation parses the rest.
///
/// # Examples
///
/// ```
/// use coblet_runtime::HttpUrl;
///
/// let url = HttpUrl::new("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")?;
/// assert!(url.is_https());
/// assert!(HttpUrl::new("file:///etc/passwd").is_err());
/// # Ok::<(), coblet_runtime::HttpUrlError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct HttpUrl(String);

/// Reason a URL was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HttpUrlError {
    /// The scheme is not `http` or `https`.
    #[error("url `{0}` must start with http:// or https://")]
    UnsupportedScheme(String),
    /// Nothing follows the scheme.
    #[error("url `{0}` has no host")]
    MissingHost(String),
    /// The URL contains whitespace or a control character.
    #[error("url {0:?} contains whitespace or a control character")]
    InvalidCharacter(String),
}

impl HttpUrl {
    /// Validates a URL.
    ///
    /// # Errors
    ///
    /// Returns [`HttpUrlError`] when the scheme is not `http` / `https`, the
    /// host part is empty, or the URL contains whitespace or control characters.
    pub fn new(url: impl Into<String>) -> Result<Self, HttpUrlError> {
        let url = url.into();
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(HttpUrlError::InvalidCharacter(url));
        }
        let lower = url.to_ascii_lowercase();
        let Some(rest) = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))
        else {
            return Err(HttpUrlError::UnsupportedScheme(url));
        };
        if rest.split(['/', '?', '#']).next().is_none_or(str::is_empty) {
            return Err(HttpUrlError::MissingHost(url));
        }
        Ok(Self(url))
    }

    /// Returns the URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reports whether the scheme is `https`.
    #[must_use]
    pub fn is_https(&self) -> bool {
        self.0
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    }
}

impl fmt::Display for HttpUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for HttpUrl {
    type Err = HttpUrlError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// HTTP method.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `HEAD`.
    Head,
    /// `POST`.
    Post,
    /// `PUT`, used to select a cape.
    Put,
    /// `DELETE`, used to reset a skin.
    Delete,
}

impl Method {
    /// Returns the method name, such as `GET`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// HTTP request handed to a [`Transport`](crate::Transport).
///
/// Header names and values are passed through as given; implementations
/// must reject ones that are not valid HTTP.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{HttpUrl, Method, Request};
///
/// let request = Request::new(Method::Get, HttpUrl::new("https://example.com/a.jar")?)
///     .header("If-None-Match", "\"abc\"")
///     .resume_from(4096);
/// assert_eq!(request.headers().len(), 1);
/// assert_eq!(request.resume_offset(), Some(4096));
/// # Ok::<(), coblet_runtime::HttpUrlError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Request {
    method: Method,
    url: HttpUrl,
    headers: Vec<(String, String)>,
    body: Option<Bytes>,
    resume_offset: Option<u64>,
}

impl Request {
    /// Creates a request without headers or body.
    #[must_use]
    pub fn new(method: Method, url: HttpUrl) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: None,
            resume_offset: None,
        }
    }

    /// Creates a `GET` request.
    #[must_use]
    pub fn get(url: HttpUrl) -> Self {
        Self::new(Method::Get, url)
    }

    /// Adds a header. Repeated names are sent as repeated headers.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Sets the request body.
    #[must_use]
    pub fn body(self, body: impl Into<Bytes>) -> Self {
        Self {
            body: Some(body.into()),
            ..self
        }
    }

    /// Asks for the bytes from `offset` to the end, sent as `Range: bytes=<offset>-`.
    #[must_use]
    pub fn resume_from(self, offset: u64) -> Self {
        Self {
            resume_offset: Some(offset),
            ..self
        }
    }

    /// Returns the method.
    #[must_use]
    pub fn method(&self) -> Method {
        self.method
    }

    /// Returns the URL.
    #[must_use]
    pub fn url(&self) -> &HttpUrl {
        &self.url
    }

    /// Returns the headers in insertion order.
    #[must_use]
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Returns the body, if any.
    #[must_use]
    pub fn body_bytes(&self) -> Option<&Bytes> {
        self.body.as_ref()
    }

    /// Returns the resume offset, if any.
    #[must_use]
    pub fn resume_offset(&self) -> Option<u64> {
        self.resume_offset
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpUrl, HttpUrlError};

    #[test]
    fn validates_urls() {
        assert!(HttpUrl::new("HTTPS://Example.com").unwrap().is_https());
        assert!(!HttpUrl::new("http://example.com/x").unwrap().is_https());
        assert!(matches!(
            HttpUrl::new("ftp://x"),
            Err(HttpUrlError::UnsupportedScheme(_))
        ));
        assert!(matches!(
            HttpUrl::new("https:///path"),
            Err(HttpUrlError::MissingHost(_))
        ));
        assert!(matches!(
            HttpUrl::new("https://"),
            Err(HttpUrlError::MissingHost(_))
        ));
        assert!(matches!(
            HttpUrl::new("https://a.com/a b"),
            Err(HttpUrlError::InvalidCharacter(_))
        ));
    }
}
