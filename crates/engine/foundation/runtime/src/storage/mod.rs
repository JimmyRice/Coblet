//! File system access: the [`Storage`] trait and its default implementation.

mod api;
#[cfg(feature = "fs")]
mod local;

pub use api::{
    DirEntry, EntryKind, FileMetadata, FileReader, FileWriter, LockGuard, Storage, StorageError,
};
#[cfg(feature = "fs")]
pub use local::TokioStorage;
