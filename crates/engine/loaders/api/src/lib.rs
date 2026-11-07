//! Mod Loader extension point for coblet.
//!
//! A [`ModLoader`] lists its versions for a game version and describes what
//! installing one adds to an Instance as an [`InstallRecipe`]: components in
//! the Prism component format ([`coblet_platform::ComponentPatch`]) that the
//! Compose step of the launch pipeline applies in order. The launch layer
//! therefore never knows which Mod Loader it runs.
//!
//! Built-in and third-party loaders implement the same trait and share one
//! [`LoaderRegistry`]; the only difference is the namespace of their
//! [`ExtensionId`](coblet_types::ExtensionId). Every request a loader makes
//! goes through [`LoaderContext`], so mirrors and the metadata cache apply.
//!
//! Loaders that only read metadata can be written as data with
//! [`DataLoader`]; [`kit`] holds building blocks for loaders written as code.

#![forbid(unsafe_code)]

mod context;
mod data;
mod error;
pub mod kit;
mod loader;
#[cfg(test)]
mod mock;
mod recipe;
mod registry;
mod version;

pub use context::LoaderContext;
pub use data::DataLoader;
pub use error::LoaderError;
pub use loader::ModLoader;
pub use recipe::InstallRecipe;
pub use registry::{DetectedLoader, LoaderRegistry};
pub use version::LoaderVersion;
