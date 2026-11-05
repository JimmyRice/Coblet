//! UniFFI bindings for coblet.
//!
//! `CobletSdk` lists versions and launches games; `GameHandle` streams the
//! log, kills and waits. Foreign code can implement `LaunchCustomizer` and
//! `LaunchStep` to change launches, as Rust code does with
//! `coblet::launch::LaunchCustomizer`. Async methods run on tokio and
//! appear as `async` in Swift, `suspend` in Kotlin and `asyncio` coroutines
//! in Python. Errors are a single `CobletError` with a kind and a message.

uniffi::setup_scaffolding!();

mod customize;
mod error;
mod game;
mod sdk;

pub use customize::{LaunchCustomizer, LaunchStep, LibrarySpec, ProfileEditor, StepInfo};
pub use error::{CobletError, ErrorKind};
pub use game::{GameExitInfo, GameHandle, LogLine};
pub use sdk::{CobletSdk, LaunchMode, LaunchOptions, VersionInfo};
