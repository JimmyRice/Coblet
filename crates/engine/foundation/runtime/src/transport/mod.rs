//! HTTP client abstraction and the values every request carries.

mod client;
mod error;
mod request;
#[cfg(feature = "reqwest")]
mod reqwest_client;
mod response;
mod rewrite;
mod user_agent;

pub use client::Transport;
pub use error::TransportError;
pub use request::{HttpUrl, HttpUrlError, Method, Request};
#[cfg(feature = "reqwest")]
pub use reqwest_client::ReqwestTransport;
pub use response::{Body, MemoryBody, Response};
pub use rewrite::{PrefixRewriter, Route, UrlRewriter, UrlRewriters};
pub use user_agent::{UserAgent, UserAgentError};
