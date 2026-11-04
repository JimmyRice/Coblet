//! Builds the command line that starts Minecraft.
//!
//! The crate is pure: it reads no files and starts no processes. A
//! [`LaunchCommandBuilder`] collects the Java executable, main class,
//! identity, classpath and argument templates, substitutes `${...}`
//! placeholders and returns a [`LaunchCommand`]. Its `Display` and `Debug`
//! output never shows access tokens.
#![forbid(unsafe_code)]

mod auth;
mod builder;
mod command;
mod error;
mod options;
mod placeholder;
mod profile;
mod values;

pub use auth::{Credential, LaunchAuth, OnlineSession, offline_uuid};
pub use builder::{
    DEFAULT_ARGFILE_THRESHOLD, LaunchCommandBuilder, NeedAuth, NeedJava, NeedMain, Ready,
};
pub use command::{ArgFile, Exposed, LaunchCommand};
pub use error::CommandError;
pub use options::LaunchOptions;
pub use placeholder::{PlaceholderResolver, UnknownPlaceholder};
pub use profile::{CommandProfile, EnvChange, FeatureClause, FeatureCondition, TemplateArg};
pub use values::{AssetsLocation, JavaExecutable, MainClass, Memory, QuickPlay, Resolution};
