use super::Transport;
use crate::{BoxFuture, HttpUrl, MemoryBody, Request, Response, TransportError};

struct Echo;

impl Transport for Echo {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async move {
            let body = request.url().as_str().to_owned();
            Ok(Response::new(200, Vec::new(), MemoryBody::new(body)))
        })
    }
}

#[tokio::test]
async fn works_as_a_trait_object() {
    let transport: Box<dyn Transport> = Box::new(Echo);
    let url = HttpUrl::new("https://example.com/x").unwrap();
    let response = transport.send(Request::get(url)).await.unwrap();
    assert_eq!(
        &response.bytes(64).await.unwrap()[..],
        b"https://example.com/x"
    );
}

#[test]
fn classifies_retryable_errors() {
    assert!(TransportError::Timeout.is_retryable());
    assert!(!TransportError::InvalidRequest("bad header".to_owned()).is_retryable());
    assert!(!TransportError::BodyTooLarge { limit: 1 }.is_retryable());
}
