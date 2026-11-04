//! Value types shared by every coblet crate.
//!
//! The crate has no IO and no async code. Each type validates its input when
//! it is constructed (through `new`, `FromStr` or `TryFrom`), so holding a
//! value proves it is well formed. All types serialize with serde, most of
//! them as a single string.
//!
//! Types whose set of values grows over time (operating systems,
//! architectures, version types, hash algorithms) are open enums: a value the
//! engine has no variant for parses into an `Other` variant instead of
//! failing.
//!
//! ```
//! use coblet_types::{Arch, ExtensionId, MavenCoordinate, OsFamily, Platform};
//!
//! let loader = ExtensionId::new("coblet:fabric")?;
//! let library: MavenCoordinate = "net.fabricmc:fabric-loader:0.16.9".parse()?;
//! let platform = Platform::new("freebsd".parse::<OsFamily>()?, "amd64".parse::<Arch>()?);
//!
//! assert_eq!(loader.namespace(), "coblet");
//! assert_eq!(library.relative_path(), "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar");
//! assert_eq!(platform.arch(), &Arch::X86_64);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

mod assertions;
mod extension_id;
mod game_version;
mod hash;
mod maven;
mod platform;
mod player_name;
mod units;

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
pub use player_name::{PlayerName, PlayerNameError};
pub use units::MiB;
