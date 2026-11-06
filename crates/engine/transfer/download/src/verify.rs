use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use bytes::Bytes;
use coblet_runtime::{
    ComputedHashes, EntryKind, Expectation, FileMetadata, Storage, StorageError, StreamHasher,
};
use coblet_types::FileHash;
use serde::{Deserialize, Serialize};

/// How existing files are checked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VerifyMode {
    /// Trust a [`FileIndex`] entry whose size and modification time still
    /// match; hash only files the index does not know.
    #[default]
    Fast,
    /// Hash every file again.
    Strict,
}

/// Index of verified files: path to size, modification time and hashes.
///
/// Fast verification of an Instance with thousands of asset objects reads
/// metadata only, because unchanged files keep their size and modification
/// time. The index is kept in memory and saved as JSON with [`FileIndex::save`].
#[derive(Debug, Default)]
pub struct FileIndex {
    entries: Mutex<HashMap<PathBuf, IndexEntry>>,
    dirty: AtomicBool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IndexEntry {
    len: u64,
    modified_ns: Option<u128>,
    hashes: Vec<FileHash>,
}

fn modified_ns(metadata: &FileMetadata) -> Option<u128> {
    metadata
        .modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
}

impl FileIndex {
    /// Creates an empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads an index saved by [`FileIndex::save`]; a missing or unreadable index starts empty.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the file exists but cannot be read.
    pub async fn load(storage: &dyn Storage, path: &Path) -> Result<Self, StorageError> {
        let bytes = match storage.read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(Self::new()),
            Err(error) => return Err(error),
        };
        // A corrupt index only costs a rehash, so it is discarded.
        let entries: HashMap<PathBuf, IndexEntry> =
            serde_json::from_slice(&bytes).unwrap_or_default();
        Ok(Self {
            entries: Mutex::new(entries),
            dirty: AtomicBool::new(false),
        })
    }

    /// Writes the index if it changed since it was loaded or last saved.
    ///
    /// # Errors
    ///
    /// Returns the [`StorageError`] of writing the file.
    pub async fn save(&self, storage: &dyn Storage, path: &Path) -> Result<(), StorageError> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let json = {
            let entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            serde_json::to_vec(&*entries).unwrap_or_default()
        };
        storage.write_atomic(path, Bytes::from(json)).await
    }

    /// Returns the number of indexed files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.len())
    }

    /// Reports whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn matches(&self, path: &Path, metadata: &FileMetadata, expect: &Expectation) -> bool {
        let Ok(entries) = self.entries.lock() else {
            return false;
        };
        let Some(entry) = entries.get(path) else {
            return false;
        };
        let Expectation::Hashes { primary, extra, .. } = expect else {
            return false;
        };
        entry.len == metadata.len
            && entry.modified_ns.is_some()
            && entry.modified_ns == modified_ns(metadata)
            && expect.size().is_none_or(|size| size == entry.len)
            && std::iter::once(primary)
                .chain(extra)
                .all(|hash| entry.hashes.contains(hash))
    }

    pub(crate) fn record(&self, path: &Path, metadata: &FileMetadata, hashes: &ComputedHashes) {
        let known: Vec<FileHash> = [
            coblet_types::HashAlgorithm::Sha1,
            coblet_types::HashAlgorithm::Sha256,
            coblet_types::HashAlgorithm::Sha512,
        ]
        .iter()
        .filter_map(|algorithm| hashes.get(algorithm).cloned())
        .collect();
        if known.is_empty() {
            return;
        }
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                path.to_path_buf(),
                IndexEntry {
                    len: metadata.len,
                    modified_ns: modified_ns(metadata),
                    hashes: known,
                },
            );
            self.dirty.store(true, Ordering::Release);
        }
    }
}

/// Hashes a file with the algorithms `expect` names.
pub(crate) async fn hash_file(
    storage: &dyn Storage,
    path: &Path,
    expect: &Expectation,
) -> Result<ComputedHashes, StorageError> {
    let mut hasher = StreamHasher::for_expectation(expect);
    let mut reader = storage.open(path).await?;
    while let Some(chunk) = reader.next_chunk().await? {
        hasher.update(&chunk);
    }
    Ok(hasher.finish())
}

/// Checks a file against `expect` without downloading anything.
///
/// A missing file or a directory never matches. An [`Expectation::Unverified`]
/// file matches when its size does (or no size is known).
pub(crate) async fn verify_file(
    storage: &dyn Storage,
    index: &FileIndex,
    path: &Path,
    expect: &Expectation,
    mode: VerifyMode,
) -> Result<bool, StorageError> {
    let Some(metadata) = storage.metadata(path).await? else {
        return Ok(false);
    };
    if metadata.kind != EntryKind::File {
        return Ok(false);
    }
    if expect.size().is_some_and(|size| size != metadata.len) {
        return Ok(false);
    }
    if !expect.is_verified() {
        return Ok(true);
    }
    if mode == VerifyMode::Fast && index.matches(path, &metadata, expect) {
        return Ok(true);
    }
    let hashes = hash_file(storage, path, expect).await?;
    let ok = hashes.check(expect).is_ok();
    if ok {
        index.record(path, &metadata, &hashes);
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use coblet_runtime::{Expectation, Storage, TokioStorage};
    use coblet_types::FileHash;

    use super::{FileIndex, VerifyMode, verify_file};

    const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    #[tokio::test]
    async fn fast_mode_trusts_the_index_until_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokioStorage::new();
        let file = dir.path().join("a");
        storage.write_atomic(&file, "abc".into()).await.unwrap();
        let expect = Expectation::hash(FileHash::new("sha1", ABC_SHA1).unwrap(), Some(3));
        let index = FileIndex::new();

        assert!(
            verify_file(&storage, &index, &file, &expect, VerifyMode::Fast)
                .await
                .unwrap()
        );
        assert_eq!(index.len(), 1);
        assert!(
            verify_file(&storage, &index, &file, &expect, VerifyMode::Fast)
                .await
                .unwrap()
        );

        // Same size, different content and modification time: the index no longer matches.
        std::thread::sleep(std::time::Duration::from_millis(20));
        storage.write_atomic(&file, "abd".into()).await.unwrap();
        assert!(
            !verify_file(&storage, &index, &file, &expect, VerifyMode::Fast)
                .await
                .unwrap()
        );
        assert!(
            !verify_file(&storage, &index, &file, &expect, VerifyMode::Strict)
                .await
                .unwrap()
        );
        assert!(
            !verify_file(
                &storage,
                &index,
                &dir.path().join("missing"),
                &expect,
                VerifyMode::Fast
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokioStorage::new();
        let file = dir.path().join("a");
        storage.write_atomic(&file, "abc".into()).await.unwrap();
        let expect = Expectation::hash(FileHash::new("sha1", ABC_SHA1).unwrap(), None);
        let index = FileIndex::new();
        verify_file(&storage, &index, &file, &expect, VerifyMode::Strict)
            .await
            .unwrap();
        let saved = dir.path().join("index.json");
        index.save(&storage, &saved).await.unwrap();
        let loaded = FileIndex::load(&storage, &saved).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(
            FileIndex::load(&storage, &dir.path().join("none"))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
