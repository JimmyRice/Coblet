use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    DirEntry, EntryKind, FileMetadata, FileReader, FileWriter, LockGuard, Storage, StorageError,
};
use crate::BoxFuture;

/// Size of the chunks returned by [`FileReader::next_chunk`].
const READ_CHUNK: usize = 256 * 1024;

/// [`Storage`] on the local file system through `tokio::fs`.
///
/// Advisory locks use [`File::lock`] (`flock` on Unix, `LockFileEx` on
/// Windows) on a blocking thread, so a waiting lock does not stall the
/// async workers.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{Storage, TokioStorage};
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let dir = std::env::temp_dir().join(format!("coblet-doc-{}", std::process::id()));
/// let storage = TokioStorage::new();
/// let file = dir.join("a/b.txt");
/// storage.write_atomic(&file, "hello".into()).await?;
/// assert_eq!(&storage.read(&file).await?[..], b"hello");
/// storage.remove_dir_all(&dir).await?;
/// # Ok::<(), coblet_runtime::StorageError>(())
/// # }).unwrap();
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioStorage;

impl TokioStorage {
    /// Creates the storage.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

async fn create_parent(path: &Path) -> Result<(), StorageError> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| StorageError::io("create_dir_all", parent, e)),
        _ => Ok(()),
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()))
}

fn kind_of(file_type: std::fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

struct TokioWriter {
    file: tokio::fs::File,
    path: PathBuf,
}

impl FileWriter for TokioWriter {
    fn write(&mut self, chunk: Bytes) -> BoxFuture<'_, Result<(), StorageError>> {
        Box::pin(async move {
            self.file
                .write_all(&chunk)
                .await
                .map_err(|e| StorageError::io("write", &self.path, e))
        })
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<(), StorageError>> {
        Box::pin(async move {
            let Self { mut file, path } = *self;
            file.flush()
                .await
                .map_err(|e| StorageError::io("flush", &path, e))?;
            file.sync_all()
                .await
                .map_err(|e| StorageError::io("sync", &path, e))
        })
    }
}

struct TokioReader {
    file: tokio::fs::File,
    path: PathBuf,
}

impl FileReader for TokioReader {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, StorageError>> {
        Box::pin(async move {
            let mut buffer = BytesMut::with_capacity(READ_CHUNK);
            let read = self
                .file
                .read_buf(&mut buffer)
                .await
                .map_err(|e| StorageError::io("read", &self.path, e))?;
            Ok((read > 0).then(|| buffer.freeze()))
        })
    }
}

/// Lock held through an open file; closing the file releases the lock.
struct FileLock {
    _file: File,
    path: PathBuf,
}

impl LockGuard for FileLock {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

async fn blocking<T: Send + 'static>(
    operation: &'static str,
    path: &Path,
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T, StorageError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| StorageError::io(operation, path, io::Error::other(e)))?
        .map_err(|e| StorageError::io(operation, path, e))
}

impl Storage for TokioStorage {
    fn read<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Bytes, StorageError>> {
        Box::pin(async move {
            tokio::fs::read(path)
                .await
                .map(Bytes::from)
                .map_err(|e| StorageError::io("read", path, e))
        })
    }

    fn open<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn FileReader>, StorageError>> {
        Box::pin(async move {
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|e| StorageError::io("open", path, e))?;
            Ok(Box::new(TokioReader {
                file,
                path: path.to_path_buf(),
            }) as Box<dyn FileReader>)
        })
    }

    fn write_atomic<'a>(
        &'a self,
        path: &'a Path,
        contents: Bytes,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            create_parent(path).await?;
            let temp = temp_sibling(path);
            let mut writer = Box::new(TokioWriter {
                file: tokio::fs::File::create(&temp)
                    .await
                    .map_err(|e| StorageError::io("create", &temp, e))?,
                path: temp.clone(),
            });
            let written = async {
                writer.write(contents).await?;
                writer.finish().await?;
                tokio::fs::rename(&temp, path)
                    .await
                    .map_err(|e| StorageError::io("rename", &temp, e))
            }
            .await;
            if written.is_err() {
                // The temporary file is useless once the write failed.
                let _ = tokio::fs::remove_file(&temp).await;
            }
            written
        })
    }

    fn create<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn FileWriter>, StorageError>> {
        Box::pin(async move {
            create_parent(path).await?;
            let file = tokio::fs::File::create(path)
                .await
                .map_err(|e| StorageError::io("create", path, e))?;
            Ok(Box::new(TokioWriter {
                file,
                path: path.to_path_buf(),
            }) as Box<dyn FileWriter>)
        })
    }

    fn append<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn FileWriter>, StorageError>> {
        Box::pin(async move {
            let file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .await
                .map_err(|e| StorageError::io("append", path, e))?;
            Ok(Box::new(TokioWriter {
                file,
                path: path.to_path_buf(),
            }) as Box<dyn FileWriter>)
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a Path,
        to: &'a Path,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            create_parent(to).await?;
            tokio::fs::rename(from, to)
                .await
                .map_err(|e| StorageError::io("rename", from, e))
        })
    }

    fn hard_link<'a>(
        &'a self,
        original: &'a Path,
        link: &'a Path,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            create_parent(link).await?;
            tokio::fs::hard_link(original, link)
                .await
                .map_err(|e| StorageError::io("hard_link", original, e))
        })
    }

    fn copy<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            create_parent(to).await?;
            tokio::fs::copy(from, to)
                .await
                .map(drop)
                .map_err(|e| StorageError::io("copy", from, e))
        })
    }

    fn symlink<'a>(
        &'a self,
        target: &'a Path,
        link: &'a Path,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            create_parent(link).await?;
            #[cfg(unix)]
            let result = tokio::fs::symlink(target, link).await;
            #[cfg(windows)]
            let result = tokio::fs::symlink_file(target, link).await;
            #[cfg(not(any(unix, windows)))]
            let result: io::Result<()> = Err(io::Error::from(io::ErrorKind::Unsupported));
            result.map_err(|e| StorageError::io("symlink", link, e))
        })
    }

    fn set_executable<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = tokio::fs::metadata(path)
                    .await
                    .map_err(|e| StorageError::io("metadata", path, e))?;
                let mut permissions = metadata.permissions();
                permissions.set_mode(permissions.mode() | 0o111);
                tokio::fs::set_permissions(path, permissions)
                    .await
                    .map_err(|e| StorageError::io("set_permissions", path, e))?;
            }
            #[cfg(not(unix))]
            let _ = path;
            Ok(())
        })
    }

    fn remove_file<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            match tokio::fs::remove_file(path).await {
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    Err(StorageError::io("remove_file", path, e))
                }
                _ => Ok(()),
            }
        })
    }

    fn remove_dir_all<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            match tokio::fs::remove_dir_all(path).await {
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    Err(StorageError::io("remove_dir_all", path, e))
                }
                _ => Ok(()),
            }
        })
    }

    fn create_dir_all<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(path)
                .await
                .map_err(|e| StorageError::io("create_dir_all", path, e))
        })
    }

    fn metadata<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Option<FileMetadata>, StorageError>> {
        Box::pin(async move {
            match tokio::fs::symlink_metadata(path).await {
                Ok(metadata) => Ok(Some(FileMetadata {
                    kind: kind_of(metadata.file_type()),
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                })),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(StorageError::io("metadata", path, e)),
            }
        })
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Vec<DirEntry>, StorageError>> {
        Box::pin(async move {
            let mut entries = tokio::fs::read_dir(path)
                .await
                .map_err(|e| StorageError::io("read_dir", path, e))?;
            let mut listed = Vec::new();
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| StorageError::io("read_dir", path, e))?
            {
                let kind = entry
                    .file_type()
                    .await
                    .map_err(|e| StorageError::io("read_dir", entry.path(), e))?;
                listed.push(DirEntry {
                    path: entry.path(),
                    kind: kind_of(kind),
                });
            }
            Ok(listed)
        })
    }

    fn lock<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn LockGuard>, StorageError>> {
        Box::pin(async move {
            let owned = path.to_path_buf();
            let file = blocking("lock", path, move || {
                let file = open_lock_file(&owned)?;
                file.lock()?;
                Ok(file)
            })
            .await?;
            Ok(Box::new(FileLock {
                _file: file,
                path: path.to_path_buf(),
            }) as Box<dyn LockGuard>)
        })
    }

    fn try_lock<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Option<Box<dyn LockGuard>>, StorageError>> {
        Box::pin(async move {
            let owned = path.to_path_buf();
            let file = blocking("try_lock", path, move || {
                let file = open_lock_file(&owned)?;
                match file.try_lock() {
                    Ok(()) => Ok(Some(file)),
                    Err(TryLockError::WouldBlock) => Ok(None),
                    Err(TryLockError::Error(e)) => Err(e),
                }
            })
            .await?;
            Ok(file.map(|file| {
                Box::new(FileLock {
                    _file: file,
                    path: path.to_path_buf(),
                }) as Box<dyn LockGuard>
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use bytes::Bytes;

    use super::TokioStorage;
    use crate::{EntryKind, Storage};

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn writes_atomically_and_reads_back() {
        let dir = temp();
        let storage = TokioStorage::new();
        let file = dir.path().join("nested/dir/file.txt");
        storage
            .write_atomic(&file, Bytes::from("one"))
            .await
            .unwrap();
        storage
            .write_atomic(&file, Bytes::from("two"))
            .await
            .unwrap();
        assert_eq!(&storage.read(&file).await.unwrap()[..], b"two");
        let listed = storage.read_dir(file.parent().unwrap()).await.unwrap();
        assert_eq!(listed.len(), 1, "temporary files were left behind");
        assert_eq!(listed[0].kind, EntryKind::File);
    }

    #[tokio::test]
    async fn streams_create_append_and_open() {
        let dir = temp();
        let storage = TokioStorage::new();
        let file = dir.path().join("a.part");
        let mut writer = storage.create(&file).await.unwrap();
        writer.write(Bytes::from("ab")).await.unwrap();
        writer.finish().await.unwrap();
        let mut writer = storage.append(&file).await.unwrap();
        writer.write(Bytes::from("cd")).await.unwrap();
        writer.finish().await.unwrap();

        let mut reader = storage.open(&file).await.unwrap();
        let mut collected = Vec::new();
        while let Some(chunk) = reader.next_chunk().await.unwrap() {
            collected.extend_from_slice(&chunk);
        }
        assert_eq!(collected, b"abcd");
        assert_eq!(storage.metadata(&file).await.unwrap().unwrap().len, 4);
    }

    #[tokio::test]
    async fn moves_links_and_removes() {
        let dir = temp();
        let storage = TokioStorage::new();
        let a = dir.path().join("a");
        let b = dir.path().join("sub/b");
        storage.write_atomic(&a, Bytes::from("x")).await.unwrap();
        storage.rename(&a, &b).await.unwrap();
        assert!(storage.metadata(&a).await.unwrap().is_none());
        storage.hard_link(&b, &a).await.unwrap();
        storage.copy(&b, &dir.path().join("c")).await.unwrap();
        storage
            .symlink(Path::new("sub/b"), &dir.path().join("link"))
            .await
            .unwrap();
        let link = storage.metadata(&dir.path().join("link")).await.unwrap();
        assert_eq!(link.unwrap().kind, EntryKind::Symlink);
        storage.set_executable(&b).await.unwrap();
        storage.remove_file(&a).await.unwrap();
        storage.remove_file(&a).await.unwrap();
        storage
            .remove_dir_all(&dir.path().join("sub"))
            .await
            .unwrap();
        storage
            .remove_dir_all(&dir.path().join("sub"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn locks_are_exclusive_until_dropped() {
        let dir = temp();
        let storage = TokioStorage::new();
        let path = dir.path().join("instance.lock");
        let held = storage.lock(&path).await.unwrap();
        assert_eq!(held.path(), path);
        assert!(storage.try_lock(&path).await.unwrap().is_none());
        drop(held);
        assert!(storage.try_lock(&path).await.unwrap().is_some());
    }
}
