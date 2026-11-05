//! UniFFI bindings for coblet.
//!
//! `CobletBuilder` adds Platform Profiles and launch customizers;
//! `Coblet` lists versions and launches games; `GameHandle` streams the
//! log, kills and waits. Foreign code can implement `PlatformProfile` to
//! adapt launches to a platform, and `LaunchCustomizer` and `LaunchStep` to
//! change single launches, as Rust code does with the traits of the same
//! names. Async methods run on tokio and
//! appear as `async` in Swift, `suspend` in Kotlin and `asyncio` coroutines
//! in Python. Errors are a single `CobletError` with a kind and a message.

uniffi::setup_scaffolding!();

mod builder;
mod context;
mod customize;
mod error;
mod game;
mod profile;

pub use builder::CobletBuilder;
pub use context::{Coblet, LaunchMode, LaunchOptions, PlatformInfo, VersionInfo};
pub use customize::{LaunchCustomizer, LaunchStep, LibrarySpec, ProfileEditor, StepInfo};
pub use error::{CobletError, ErrorKind};
pub use game::{GameExitInfo, GameHandle, LogLine};
pub use profile::{
    ComponentInfo, JavaSourceChoice, LibraryInfo, NativesChoice, PlatformProfile, ProfileDraft,
    ProfileTarget, RuleOverrides,
};
