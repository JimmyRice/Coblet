//! Value types shared by every coblet crate.
//!
//! The crate has no IO and no async code. Every type here is plain data that
//! validates itself on construction.

#![forbid(unsafe_code)]

mod extension_id;
mod game_version;
mod hash;
mod maven;
mod platform;

pub use extension_id::{ExtensionId, ExtensionIdError};
pub use game_version::{GameVersionId, GameVersionIdError, VersionType};
pub use hash::{
    Digest, DigestError, FileHash, FileHashError, HashAlgorithm, Sha1Digest, Sha256Digest,
    Sha512Digest,
};
pub use maven::{MavenCoordinate, MavenCoordinateError};
pub use platform::{
    Arch, ArchName, Libc, LibcName, OsFamily, OsName, Platform, PlatformNameError, PlatformTrait,
};
