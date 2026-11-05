use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::http::query_pairs;
use crate::AuthError;

/// Largest request head read from a browser.
const REQUEST_LIMIT: usize = 16 * 1024;

/// How long a browser connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Login</title>\
<p>The login is complete. You can close this window and return to the launcher.</p>";

/// Listener on the loopback interface that receives the browser redirect.
#[derive(Debug)]
pub(crate) struct Loopback {
    listener: TcpListener,
    origin: String,
    path: String,
    redirect_uri: String,
}

impl Loopback {
    /// Binds to the loopback interface. `host` is `127.0.0.1`, `localhost`
    /// or `[::1]` and only changes the redirect URI and the address family.
    pub(crate) async fn bind(host: &str, port: u16, path: &str) -> Result<Self, AuthError> {
        if !path.starts_with('/') || path.contains(['?', '#']) {
            return Err(AuthError::InvalidOptions(format!(
                "redirect path {path:?} must start with / and have no query"
            )));
        }
        let address = match host {
            "127.0.0.1" | "localhost" => SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            "[::1]" => SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
            other => {
                return Err(AuthError::InvalidOptions(format!(
                    "redirect host {other:?} is not 127.0.0.1, localhost or [::1]"
                )));
            }
        };
        let failed = |error: std::io::Error| {
            AuthError::Failed(format!("cannot listen for the redirect: {error}"))
        };
        let listener = TcpListener::bind(address).await.map_err(failed)?;
        let port = listener.local_addr().map_err(failed)?.port();
        let origin = format!("http://{host}:{port}");
        Ok(Self {
            redirect_uri: format!("{origin}{path}"),
            listener,
            origin,
            path: path.to_owned(),
        })
    }

    /// Returns the redirect URI to register with the login.
    pub(crate) fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Serves connections until one carries the redirect, then returns its
    /// full URI. Other requests, such as a favicon, get a 404.
    pub(crate) async fn next_redirect(&self) -> String {
        loop {
            let Ok((stream, _)) = self.listener.accept().await else {
                continue;
            };
            let handled = tokio::time::timeout(READ_TIMEOUT, self.handle(stream)).await;
            if let Ok(Some(uri)) = handled {
                return uri;
            }
        }
    }

    async fn handle(&self, mut stream: TcpStream) -> Option<String> {
        let target = read_target(&mut stream).await?;
        let path = target.split(['?', '#']).next().unwrap_or_default();
        let is_redirect = path == self.path
            && query_pairs(&target)
                .is_some_and(|pairs| pairs.iter().any(|(key, _)| key == "code" || key == "error"));
        let response = if is_redirect {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{DONE_PAGE}",
                DONE_PAGE.len()
            )
        } else {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        };
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        is_redirect.then(|| format!("{}{target}", self.origin))
    }
}

/// Reads the request head and returns the target of a `GET`.
async fn read_target(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut chunk = [0_u8; 2048];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 || head.len() + read > REQUEST_LIMIT {
            return None;
        }
        head.extend_from_slice(&chunk[..read]);
    }
    let head = std::str::from_utf8(&head).ok()?;
    let mut parts = head.lines().next()?.split(' ');
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) if target.starts_with('/') => Some(target.to_owned()),
        _ => None,
    }
}
