//! Value types shared by every coblet crate.
//!
//! The crate has no IO and no async code. Every type here is plain data that
//! validates itself on construction.

#![forbid(unsafe_code)]

mod extension_id;
mod game_version;
mod maven;

pub use extension_id::{ExtensionId, ExtensionIdError};
pub use game_version::{GameVersionId, GameVersionIdError, VersionType};
pub use maven::{MavenCoordinate, MavenCoordinateError};
