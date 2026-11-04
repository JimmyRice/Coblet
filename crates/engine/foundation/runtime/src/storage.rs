use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bytes::Bytes;

use crate::BoxFuture;

/// File system access used by every crate that reads or writes files.
///
/// Paths are absolute or relative to the process working directory; the
/// trait does not sandbox them. Callers build paths from validated parts
/// (such as [`coblet_types::MavenCoordinate::relative_path`]) so metadata
/// cannot steer writes outside the data root.
///
/// The default implementation on `tokio::fs` comes with the downloader.
pub trait Storage: Send + Sync + 'static {
    /// Reads a whole file.
    fn read<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Bytes, StorageError>>;

    /// Writes a whole file atomically: readers see the old content or the new
    /// content, never a mix. Parent directories are created as needed.
    fn write_atomic<'a>(
        &'a self,
        path: &'a Path,
        contents: Bytes,
    ) -> BoxFuture<'a, Result<(), StorageError>>;

    /// Creates or truncates a file for streaming writes, creating parent directories.
    fn create<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn FileWriter>, StorageError>>;

    /// Opens an existing file for appending, used to resume a `.part` download.
    fn append<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn FileWriter>, StorageError>>;

    /// Renames a file or directory, replacing a file at `to`.
    fn rename<'a>(
        &'a self,
        from: &'a Path,
        to: &'a Path,
    ) -> BoxFuture<'a, Result<(), StorageError>>;

    /// Creates a hard link at `link` pointing to `original`.
    fn hard_link<'a>(
        &'a self,
        original: &'a Path,
        link: &'a Path,
    ) -> BoxFuture<'a, Result<(), StorageError>>;

    /// Removes a file. Removing a missing file succeeds.
    fn remove_file<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), StorageError>>;

    /// Removes a directory and everything below it. Removing a missing directory succeeds.
    fn remove_dir_all<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), StorageError>>;

    /// Creates a directory and its missing parents.
    fn create_dir_all<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), StorageError>>;

    /// Returns metadata, or `None` when nothing exists at `path`. Symbolic links are not followed.
    fn metadata<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Option<FileMetadata>, StorageError>>;

    /// Lists the direct children of a directory, in no particular order.
    fn read_dir<'a>(&'a self, path: &'a Path)
    -> BoxFuture<'a, Result<Vec<DirEntry>, StorageError>>;

    /// Takes an exclusive advisory lock on `path`, creating the file if needed,
    /// and waits while another process holds it. Dropping the guard releases it.
    fn lock<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn LockGuard>, StorageError>>;

    /// Takes the lock like [`Storage::lock`] but returns `None` instead of waiting.
    fn try_lock<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Option<Box<dyn LockGuard>>, StorageError>>;
}

/// Streaming writer returned by [`Storage::create`] and [`Storage::append`].
///
/// Dropping a writer without calling [`FileWriter::finish`] leaves the bytes
/// written so far on disk without a durability guarantee.
pub trait FileWriter: Send {
    /// Appends a chunk.
    fn write(&mut self, chunk: Bytes) -> BoxFuture<'_, Result<(), StorageError>>;

    /// Flushes and syncs the file to disk.
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<(), StorageError>>;
}

/// Held advisory lock; dropping it releases the lock.
pub trait LockGuard: Send + Sync {
    /// Returns the locked path.
    fn path(&self) -> &Path;
}

/// Kind of a file system entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link, not followed.
    Symlink,
    /// Anything else, such as a socket or device.
    Other,
}

/// Size, kind and modification time of a file system entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    /// Kind of the entry.
    pub kind: EntryKind,
    /// Size in bytes; for directories the value is platform specific.
    pub len: u64,
    /// Last modification time, when the platform reports one.
    pub modified: Option<SystemTime>,
}

/// Child of a directory listed by [`Storage::read_dir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// Full path of the child.
    pub path: PathBuf,
    /// Kind of the child, not following symbolic links.
    pub kind: EntryKind,
}

/// Failure of a [`Storage`] operation, with the path it concerned.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The operating system reported an error.
    #[error("{operation} failed for {path}: {source}")]
    Io {
        /// Short name of the operation, such as `rename`.
        operation: &'static str,
        /// The path involved; for two-path operations, the source.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}

impl StorageError {
    /// Wraps an IO error with the operation and path it came from.
    #[must_use]
    pub fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    /// Reports whether the error means the path does not exist.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        match self {
            Self::Io { source, .. } => source.kind() == io::ErrorKind::NotFound,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::StorageError;

    #[test]
    fn reports_operation_and_path() {
        let error = StorageError::io(
            "rename",
            "/data/a.part",
            io::Error::new(io::ErrorKind::NotFound, "no such file"),
        );
        assert!(error.is_not_found());
        assert_eq!(
            error.to_string(),
            "rename failed for /data/a.part: no such file"
        );
    }
}
