//! HTTP client abstraction and the values every request carries.

mod client;
mod error;
mod request;
mod response;
mod user_agent;

pub use client::Transport;
pub use error::TransportError;
pub use request::{HttpUrl, HttpUrlError, Method, Request};
pub use response::{Body, MemoryBody, Response};
pub use user_agent::{UserAgent, UserAgentError};
