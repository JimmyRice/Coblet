use bytes::Bytes;

use super::{Body, MemoryBody, Response};
use crate::{BoxFuture, TransportError};

struct Chunks(Vec<&'static [u8]>);

impl Body for Chunks {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, TransportError>> {
        let next = (!self.0.is_empty()).then(|| Bytes::from_static(self.0.remove(0)));
        Box::pin(async move { Ok(next) })
    }
}

#[tokio::test]
async fn collects_chunks_up_to_the_limit() {
    let response = Response::new(200, Vec::new(), Chunks(vec![b"ab", b"cd"]));
    assert_eq!(&response.bytes(4).await.unwrap()[..], b"abcd");

    let response = Response::new(200, Vec::new(), Chunks(vec![b"ab", b"cd"]));
    assert!(matches!(
        response.bytes(3).await,
        Err(TransportError::BodyTooLarge { limit: 3 })
    ));
}

#[tokio::test]
async fn memory_body_yields_once() {
    let mut response = Response::new(404, Vec::new(), MemoryBody::new("x"));
    assert!(!response.is_success());
    assert_eq!(
        response.next_chunk().await.unwrap().as_deref(),
        Some(&b"x"[..])
    );
    assert_eq!(response.next_chunk().await.unwrap(), None);
    let mut empty = Response::new(204, Vec::new(), MemoryBody::default());
    assert_eq!(empty.next_chunk().await.unwrap(), None);
}
