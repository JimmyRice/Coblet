use coblet_runtime::{BoxFuture, Request, Response, Transport, TransportError};

/// Transport that answers every request with 404.
pub(crate) struct NoTransport;

impl Transport for NoTransport {
    fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async {
            Ok(Response::new(
                404,
                Vec::new(),
                coblet_runtime::MemoryBody::new(""),
            ))
        })
    }
}
