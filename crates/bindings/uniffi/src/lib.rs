//! UniFFI bindings for coblet.
//!
//! `Coblet` lists versions and launches games; `GameHandle` streams the
//! log, kills and waits. Foreign code can implement `LaunchCustomizer` and
//! `LaunchStep` to change launches, as Rust code does with
//! `coblet::launch::LaunchCustomizer`. Async methods run on tokio and
//! appear as `async` in Swift, `suspend` in Kotlin and `asyncio` coroutines
//! in Python. Errors are a single `CobletError` with a kind and a message.

uniffi::setup_scaffolding!();

mod context;
mod customize;
mod error;
mod game;

pub use context::{Coblet, LaunchMode, LaunchOptions, VersionInfo};
pub use customize::{LaunchCustomizer, LaunchStep, LibrarySpec, ProfileEditor, StepInfo};
pub use error::{CobletError, ErrorKind};
pub use game::{GameExitInfo, GameHandle, LogLine};
