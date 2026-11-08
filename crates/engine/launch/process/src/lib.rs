//! Prepares, starts and monitors Minecraft.
//!
//! A [`Launcher`] runs a chain of [`LaunchStep`]s on a [`LaunchContext`]:
//! select Java, download and verify files, extract natives, choose the
//! client jar, add the log configuration, run the pre-launch command, build
//! the command with `coblet-launch-command` and start the process. The
//! returned [`GameSession`] streams parsed and redacted log lines, keeps the
//! most recent ones for late subscribers and reports a [`GameExit`].
#![forbid(unsafe_code)]

mod buffer;
mod convert;
mod customize;
mod diff;
mod error;
mod files;
mod launcher;
mod layout;
mod log;
mod natives;
mod placeholder_sources;
mod placeholders;
mod request;
mod session;
mod step;
pub mod step_names;
mod steps;

pub use buffer::{DEFAULT_LOG_CAPACITY, LogBuffer, LogItem, LogStream};
pub use convert::command_profile;
pub use customize::{CustomizeCtx, LaunchCustomizer};
pub use error::LaunchError;
pub use launcher::Launcher;
pub use layout::GameLayout;
pub use log::{LogParser, Redact};
pub use placeholder_sources::{PlaceholderCtx, PlaceholderSource, PlaceholderSources};
pub use placeholders::{PlaceholderOrigin, Placeholders, check_placeholder_name};
pub use request::{LaunchHook, LaunchRequest};
pub use session::{GameExit, GameKiller, GameSession};
pub use step::{LaunchContext, LaunchServices, LaunchStep};
pub use steps::{
    BuildCommand, ClientJar, LegacyFmlLibs, LoggingConfig, PreLaunch, PrepareNatives,
    ResolvePlaceholders, SelectJava, Spawn, VerifyFiles, builtin_steps,
};
