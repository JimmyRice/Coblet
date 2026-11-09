//! Minecraft launcher library.
//!
//! [`Coblet`] ties the subsystems together: the version catalog, Java
//! discovery and runtime downloads, platform profiles and the launcher.
//! Each subsystem crate is re-exported, so downstream code can also use
//! them without the `Coblet`.
//!
//! Accounts are always available: offline accounts, the account store and
//! third-party auth methods.
//!
//! Every source can be replaced or extended:
//! [`CobletBuilder::url_rewriter`] points downloads and metadata at
//! mirrors, [`CobletBuilder::version_source`] and
//! [`CobletBuilder::version_manifest`] add game versions after the Mojang
//! manifest, and [`CobletBuilder::without_version_source`] can drop the
//! manifest itself. Mod Loaders are registered the same way, built-in or
//! not: [`CobletBuilder::loader`] for code, [`CobletBuilder::loader_toml`]
//! for data, and [`CobletBuilder::replace_loader`] to swap one; a loader
//! gets versions from more places with
//! [`CobletBuilder::loader_version_source`] or
//! [`CobletBuilder::loader_version_source_toml`]. Java follows the same
//! pattern: [`CobletBuilder::java_provider`] and
//! [`CobletBuilder::java_provider_toml`] add download sources after
//! Mojang, Adoptium and Azul Zulu, and [`CobletBuilder::java_locator`] adds
//! places to find installed Java.
//! Version traits, which tell launchers and Platform Profiles what is
//! special about a version, come from the same kind of list:
//! [`CobletBuilder::trait_table_toml`] adds rules as data,
//! [`CobletBuilder::trait_source`] adds code, and
//! [`CobletBuilder::replace_trait_source`] or
//! [`CobletBuilder::without_trait_source`] change the built-in table.
//!
//! Features: `mojang`, `java`, `launch` and `loaders` (all default) select subsystems;
//! `tls-rustls` (default) provides the default transport. `auth-msa` adds
//! Microsoft login, skins and capes, `auth-loopback` its built-in redirect
//! listener and `encrypted-secrets` an encrypted token file.
#![forbid(unsafe_code)]

mod accounts;
#[cfg(feature = "appearance")]
mod appearance;
mod builder;
mod context;
mod error;
#[cfg(feature = "java")]
mod java_runtimes;
#[cfg(feature = "launch")]
mod launching;
#[cfg(feature = "loaders")]
mod loaders;
#[cfg(feature = "mojang")]
mod versions;

pub use builder::{CobletBuilder, NoRoot, WithRoot};
pub use context::Coblet;
pub use error::Error;

pub use coblet_auth as auth;
pub use coblet_catalog as catalog;
pub use coblet_download as download;
#[cfg(feature = "java")]
pub use coblet_java as java;
#[cfg(feature = "launch")]
pub use coblet_launch as launch;
#[cfg(feature = "launch")]
pub use coblet_launch_command as launch_command;
#[cfg(feature = "loaders")]
pub use coblet_loader as loader;
#[cfg(feature = "mojang")]
pub use coblet_mojang as mojang;
#[cfg(feature = "launch")]
pub use coblet_platform as platform;
#[cfg(feature = "appearance")]
pub use coblet_player as player;
pub use coblet_runtime as runtime;
pub use coblet_types as types;
