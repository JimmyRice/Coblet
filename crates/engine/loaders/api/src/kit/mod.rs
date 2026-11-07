//! Building blocks for Mod Loaders, open to third-party loaders: Maven
//! addresses and metadata.

mod maven;

pub use maven::{MavenMetadata, artifact_url, metadata_url};
