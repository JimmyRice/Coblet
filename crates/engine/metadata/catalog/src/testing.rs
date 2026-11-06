//! Scripted transport shared by the unit tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use coblet_runtime::{BoxFuture, MemoryBody, Request, Response, Transport, TransportError};

pub(crate) enum Reply {
    Ok(u16, Vec<(&'static str, &'static str)>, &'static str),
    Fail,
}

#[derive(Default)]
pub(crate) struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    pub(crate) requests: Mutex<Vec<Request>>,
}

impl Scripted {
    pub(crate) fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::default(),
        }
    }

    pub(crate) fn push(&self, reply: Reply) {
        self.replies.lock().unwrap().push_back(reply);
    }

    pub(crate) fn header_of(&self, index: usize, name: &str) -> Option<String> {
        self.requests.lock().unwrap()[index]
            .headers()
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    pub(crate) fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Transport for Scripted {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front();
        Box::pin(async move {
            match reply {
                Some(Reply::Ok(status, headers, body)) => Ok(Response::new(
                    status,
                    headers
                        .into_iter()
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .collect(),
                    MemoryBody::new(body),
                )),
                Some(Reply::Fail) | None => Err(TransportError::Timeout),
            }
        })
    }
}
