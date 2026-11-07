//! Transport that serves fixed documents by address, for the unit tests.

use std::collections::BTreeMap;
use std::sync::Mutex;

use coblet_runtime::{BoxFuture, MemoryBody, Request, Response, Transport, TransportError};

/// Serves each address its document and answers 404 for any other; records the addresses asked for.
pub(crate) struct Served {
    documents: BTreeMap<String, String>,
    pub(crate) asked: Mutex<Vec<String>>,
}

impl Served {
    pub(crate) fn new<'a>(documents: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            documents: documents
                .into_iter()
                .map(|(url, body)| (url.to_owned(), body.to_owned()))
                .collect(),
            asked: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Transport for Served {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let url = request.url().as_str().to_owned();
        self.asked.lock().unwrap().push(url.clone());
        let found = self.documents.get(&url).cloned();
        Box::pin(async move {
            Ok(match found {
                Some(body) => Response::new(200, Vec::new(), MemoryBody::new(body)),
                None => Response::new(404, Vec::new(), MemoryBody::new("")),
            })
        })
    }
}
