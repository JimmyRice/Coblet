//! UniFFI bindings for coblet.
//!
//! `CobletSdk` lists versions and launches games; `GameHandle` streams the
//! log, kills and waits. Async methods run on tokio and appear as `async`
//! in Swift, `suspend` in Kotlin and `asyncio` coroutines in Python. Errors
//! are a single `CobletError` with a kind and a message.

uniffi::setup_scaffolding!();

mod error;
mod game;
mod sdk;

pub use error::{CobletError, ErrorKind};
pub use game::{GameExitInfo, GameHandle, LogLine};
pub use sdk::{CobletSdk, LaunchMode, VersionInfo};
