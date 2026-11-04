//! Java runtimes for coblet.
//!
//! [`discover`] finds installed Java runtimes (configured paths, installed
//! Mojang runtimes, `JAVA_HOME`, `PATH`, SDKMAN! and the platform's JDK
//! folders) and [`probe`]s each one for its version and architecture.
//! [`select`] picks one for a
//! [`JavaRequirement`](coblet_platform::JavaRequirement) and
//! [`RuntimeInstaller`] downloads the Mojang runtime the requirement names.
//! [`find_or_install`] combines the three.

#![forbid(unsafe_code)]

mod discover;
mod error;
mod install;
mod probe;
mod runtime;
mod select;

pub use discover::{SearchPaths, candidates, discover, executable_name};
pub use error::JavaError;
pub use install::{InstallSource, JavaInstall, major_version};
pub use probe::probe;
pub use runtime::{
    JavaContext, ManifestRef, RUNTIME_INDEX_URL, RuntimeBuild, RuntimeDownloads, RuntimeEntry,
    RuntimeFile, RuntimeIndex, RuntimeInstaller, RuntimeManifest, RuntimeVersion, find_or_install,
};
pub use select::select;
