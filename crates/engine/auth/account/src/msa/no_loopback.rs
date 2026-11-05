use std::future::{Future, ready};

use crate::AuthError;

/// Stand-in for the loopback listener in builds without the `loopback` feature.
#[derive(Debug)]
pub(crate) enum Loopback {}

impl Loopback {
    pub(crate) fn bind(
        _host: &str,
        _port: u16,
        _path: &str,
    ) -> impl Future<Output = Result<Self, AuthError>> {
        ready(Err(AuthError::Unsupported(
            "the loopback redirect needs the `loopback` feature".to_owned(),
        )))
    }

    pub(crate) fn redirect_uri(&self) -> &str {
        match *self {}
    }

    pub(crate) fn next_redirect(&self) -> std::future::Pending<String> {
        match *self {}
    }
}
