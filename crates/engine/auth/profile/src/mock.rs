//! Scripted HTTP services for tests.

use std::sync::{Arc, Mutex};

use coblet_runtime::{BoxFuture, MemoryBody, Method, Request, Response, Transport, TransportError};

/// Request seen by the mock.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Seen {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Scripted reply: method, exact URL, status and body.
type Route = (Method, String, u16, Vec<u8>);

/// Transport answering by method and exact URL.
#[derive(Default)]
pub(crate) struct Mock {
    routes: Mutex<Vec<Route>>,
    seen: Mutex<Vec<Seen>>,
}

impl Mock {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn on(
        &self,
        method: Method,
        url: &str,
        status: u16,
        body: impl Into<Vec<u8>>,
    ) -> &Self {
        self.routes
            .lock()
            .unwrap()
            .push((method, url.to_owned(), status, body.into()));
        self
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn seen_with(&self, method: Method) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|seen| seen.method == method)
            .collect()
    }
}

impl Transport for Mock {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let url = request.url().as_str().to_owned();
        self.seen.lock().unwrap().push(Seen {
            method: request.method(),
            url: url.clone(),
            headers: request.headers().to_vec(),
            body: request.body_bytes().map(|b| b.to_vec()).unwrap_or_default(),
        });
        let reply = self
            .routes
            .lock()
            .unwrap()
            .iter()
            .find(|(method, route, _, _)| *method == request.method() && *route == url)
            .map(|(_, _, status, body)| (*status, body.clone()));
        let (status, body) = reply.unwrap_or((418, b"no route".to_vec()));
        Box::pin(async move { Ok(Response::new(status, Vec::new(), MemoryBody::new(body))) })
    }
}
