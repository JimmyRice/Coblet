use std::sync::Arc;

use coblet_runtime::{HttpUrl, Request, Transport};
use serde::de::DeserializeOwned;

use crate::PlayerError;

const BODY_LIMIT: u64 = 1 << 20;
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

    pub(crate) fn json<T: DeserializeOwned>(&self) -> Result<T, PlayerError> {
        serde_json::from_slice(&self.body).map_err(|error| PlayerError::InvalidResponse {
            endpoint: self.endpoint,
            message: error.to_string(),
        })
    }

    /// Maps a non-success status to an error.
    pub(crate) fn error(&self) -> PlayerError {
        if self.status == 401 {
            return PlayerError::Unauthorized;
        }
        let text = String::from_utf8_lossy(&self.body);
        let text = text.trim();
        let message = match text.char_indices().nth(MESSAGE_LIMIT) {
            Some((cut, _)) => format!("{}...", &text[..cut]),
            None => text.to_owned(),
        };
        PlayerError::Http {
            endpoint: self.endpoint,
            status: self.status,
            message,
        }
    }
}

pub(crate) async fn send(
    transport: &Arc<dyn Transport>,
    endpoint: &'static str,
    request: Request,
    limit: Option<u64>,
) -> Result<Reply, PlayerError> {
    let failed = |source| PlayerError::Transport { endpoint, source };
    let response = transport.send(request).await.map_err(failed)?;
    let status = response.status();
    let body = response
        .bytes(limit.unwrap_or(BODY_LIMIT))
        .await
        .map_err(failed)?;
    Ok(Reply {
        endpoint,
        status,
        body: body.to_vec(),
    })
}

pub(crate) fn url(url: &str) -> Result<HttpUrl, PlayerError> {
    HttpUrl::new(url).map_err(|error| PlayerError::InvalidResponse {
        endpoint: "url",
        message: error.to_string(),
    })
}
