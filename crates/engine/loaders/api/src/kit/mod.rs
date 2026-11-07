//! Building blocks for Mod Loaders, open to third-party loaders: Maven
//! addresses and metadata, and turning `client.json`-style loader profiles
//! into components.

mod maven;
mod version_json;

pub use maven::{MavenMetadata, artifact_url, metadata_url};
pub use version_json::{SplitRule, VersionJsonImport};
