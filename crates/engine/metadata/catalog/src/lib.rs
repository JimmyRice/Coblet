//! Version catalogs for coblet: game versions, Mod Loader versions and
//! server builds share one [`Catalog`] abstraction.
//!
//! A [`Catalog`] merges the items of several [`CatalogSource`]s, removes
//! duplicates, sorts by release time, lets [`CatalogStage`]s change the
//! list and keeps the result in memory until [`Catalog::refresh`]. Sources fetch documents through a [`MetaCache`],
//! an HTTP cache in `data_root/cache/meta/` that revalidates with `ETag` and
//! `If-Modified-Since` and falls back to the cached copy when the network
//! fails, publishing a `catalog.offline-cache` warning.
//!
//! Version strings are opaque: since 26.1 Minecraft uses year-based
//! numbers, so catalogs order items by [`Timestamp`] and source order only.
//!
//! ```
//! use coblet_catalog::{CatalogItem, Filter, Timestamp};
//! use coblet_types::VersionType;
//!
//! #[derive(Clone)]
//! struct Build { id: String, kind: VersionType, time: Timestamp }
//!
//! impl CatalogItem for Build {
//!     fn key(&self) -> &str { &self.id }
//!     fn version_type(&self) -> &VersionType { &self.kind }
//!     fn released(&self) -> Option<Timestamp> { Some(self.time) }
//! }
//!
//! let build = Build { id: "26.3".into(), kind: VersionType::Release, time: "2026-09-01T10:00:00Z".parse()? };
//! assert!(Filter::new().with_types([VersionType::Release]).with_text("26.").matches(&build));
//! assert!(!Filter::new().with_types([VersionType::Snapshot]).matches(&build));
//! # Ok::<(), coblet_catalog::TimestampError>(())
//! ```

#![forbid(unsafe_code)]

mod catalog;
mod error;
mod item;
mod meta;
mod source;
mod sources;
#[cfg(test)]
mod testing;
mod timestamp;

pub use catalog::{Catalog, SOURCE_FAILED};
pub use error::CatalogError;
pub use item::{CatalogItem, CatalogPage, CatalogQuery, Filter};
pub use meta::{
    CachePolicy, DEFAULT_MAX_SIZE, Fetched, MetaCache, MetaRequest, OFFLINE_CACHE, Origin,
    UNVERIFIED_MIRROR,
};
pub use source::{CatalogContext, CatalogSource, CatalogStage};
pub use sources::CatalogSources;
pub use timestamp::{Timestamp, TimestampError};
