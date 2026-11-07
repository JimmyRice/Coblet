//! Java runtimes for coblet.
//!
//! [`discover`] finds installed Java runtimes (configured paths, runtimes
//! coblet installed, `JAVA_HOME`, `PATH`, SDKMAN! and the platform's JDK
//! folders) and [`probe`]s each one for its version and architecture.
//! [`select`] picks one for a
//! [`JavaRequirement`](coblet_platform::JavaRequirement). When none fits,
//! the [`JavaProviders`], such as [`MojangRuntimes`], describe packages to
//! download and the [`JavaInstaller`] installs one. [`find_or_install`]
//! combines them.

#![forbid(unsafe_code)]

mod archive;
mod discover;
mod error;
mod install;
mod installer;
mod mojang;
mod probe;
mod provider;
mod runtime;
mod select;

pub use discover::{SearchPaths, candidates, discover, executable_name};
pub use error::JavaError;
pub use install::{InstallSource, JavaInstall, major_version};
pub use installer::JavaInstaller;
pub use mojang::{MOJANG_PROVIDER_ID, MojangRuntimes};
pub use probe::probe;
pub use provider::{
    ArchiveFormat, JavaDownload, JavaPackage, JavaProvider, JavaProviders, JavaQuery,
    ProviderContext,
};
pub use runtime::{
    JavaContext, ManifestRef, RUNTIME_INDEX_URL, RuntimeBuild, RuntimeDownloads, RuntimeEntry,
    RuntimeFile, RuntimeIndex, RuntimeManifest, RuntimeVersion, find_or_install,
};
pub use select::select;
