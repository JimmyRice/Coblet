//! Minecraft launcher SDK.
//!
//! [`Sdk`] ties the subsystems together: the version catalog, Java
//! discovery and runtime downloads, platform profiles and the launcher.
//! Each subsystem crate is re-exported, so downstream code can also use
//! them without the `Sdk`.
//!
//! Features: `mojang`, `java` and `launch` (all default) select subsystems;
//! `tls-rustls` (default) provides the default transport.
#![forbid(unsafe_code)]

mod builder;
mod error;
#[cfg(feature = "launch")]
mod launching;
mod sdk;
#[cfg(feature = "mojang")]
mod versions;

pub use builder::{NoRoot, SdkBuilder, WithRoot};
pub use error::SdkError;
pub use sdk::Sdk;

pub use coblet_catalog as catalog;
pub use coblet_download as download;
#[cfg(feature = "java")]
pub use coblet_java as java;
#[cfg(feature = "launch")]
pub use coblet_launch as launch;
#[cfg(feature = "launch")]
pub use coblet_launch_command as launch_command;
#[cfg(feature = "mojang")]
pub use coblet_mojang as mojang;
#[cfg(feature = "launch")]
pub use coblet_platform as platform;
pub use coblet_runtime as runtime;
pub use coblet_types as types;
