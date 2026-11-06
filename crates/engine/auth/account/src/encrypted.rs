use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use coblet_runtime::{BoxFuture, Secret, SecretKey, SecretStore, SecretStoreError, Storage};
use zeroize::{Zeroize, Zeroizing};

/// First bytes of an encrypted secret file; also bound into the ciphertext.
const MAGIC: &[u8; 8] = b"CBLTSEC1";
const NONCE_LEN: usize = 12;

/// 256-bit key of an [`EncryptedFileStore`]. The bytes are wiped on drop
/// and never printed.
#[derive(Clone)]
pub struct EncryptionKey([u8; 32]);

impl EncryptionKey {
    /// Wraps raw key bytes, for example read from a system keychain.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Creates a random key from the operating system's random source.
    ///
    /// # Errors
    ///
    /// Returns [`SecretStoreError::Unavailable`] when no random source works.
    pub fn generate() -> Result<Self, SecretStoreError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|error| SecretStoreError::Unavailable(format!("no random source: {error}")))?;
        Ok(Self(bytes))
    }

    /// Returns the key bytes, to save them somewhere safe.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for EncryptionKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptionKey(<redacted>)")
    }
}

/// Secret store that keeps every secret in one file encrypted with
/// ChaCha20-Poly1305.
///
/// The key decides who can read the file, so keep it apart from the file,
/// for example in a system keychain. Each write uses a fresh random nonce
/// and replaces the file atomically. Key names are encrypted too.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_auth::{EncryptedFileStore, EncryptionKey};
/// use coblet_runtime::{Secret, SecretKey, SecretStore, TokioStorage};
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// # let dir = tempfile::tempdir()?;
/// let key = EncryptionKey::generate()?;
/// let store = EncryptedFileStore::new(Arc::new(TokioStorage::new()), dir.path().join("secrets.bin"), key);
/// let name = SecretKey::new("coblet:microsoft/main/refresh_token")?;
/// store.put(&name, Secret::new("M.C1")).await?;
/// assert_eq!(store.get(&name).await?.unwrap().expose(), "M.C1");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// ```
pub struct EncryptedFileStore {
    storage: Arc<dyn Storage>,
    path: PathBuf,
    cipher: ChaCha20Poly1305,
    writer: tokio::sync::Mutex<()>,
}

impl EncryptedFileStore {
    /// Creates a store for `path`; the file is created by the first write.
    #[must_use]
    pub fn new(storage: Arc<dyn Storage>, path: impl Into<PathBuf>, key: EncryptionKey) -> Self {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_bytes()));
        // The cipher keeps its own copy; wipe this one now.
        drop(key);
        Self {
            storage,
            path: path.into(),
            cipher,
            writer: tokio::sync::Mutex::new(()),
        }
    }

    /// Returns the path of the file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn load(&self) -> Result<Entries, SecretStoreError> {
        let bytes = match self.storage.read(&self.path).await {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(Entries::default()),
            Err(error) => return Err(SecretStoreError::Backend(Box::new(error))),
        };
        let damaged = || {
            SecretStoreError::Unavailable(format!(
                "{} cannot be decrypted: wrong key or damaged file",
                self.path.display()
            ))
        };
        let body = bytes.strip_prefix(MAGIC.as_slice()).ok_or_else(damaged)?;
        if body.len() < NONCE_LEN {
            return Err(damaged());
        }
        let (nonce, ciphertext) = body.split_at(NONCE_LEN);
        let plain = Zeroizing::new(
            self.cipher
                .decrypt(
                    Nonce::from_slice(nonce),
                    Payload {
                        msg: ciphertext,
                        aad: MAGIC,
                    },
                )
                .map_err(|_| damaged())?,
        );
        let map: BTreeMap<String, String> =
            serde_json::from_slice(&plain).map_err(|_| damaged())?;
        Ok(Entries(map))
    }

    async fn store(&self, entries: &Entries) -> Result<(), SecretStoreError> {
        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|error| SecretStoreError::Unavailable(format!("no random source: {error}")))?;
        let plain = Zeroizing::new(
            serde_json::to_vec(&entries.0)
                .map_err(|error| SecretStoreError::Backend(Box::new(error)))?,
        );
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain.as_slice(),
                    aad: MAGIC,
                },
            )
            .map_err(|_| SecretStoreError::Unavailable("encryption failed".to_owned()))?;
        let mut file = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ciphertext.len());
        file.extend_from_slice(MAGIC);
        file.extend_from_slice(&nonce);
        file.extend_from_slice(&ciphertext);
        if let Some(parent) = self.path.parent() {
            self.storage
                .create_dir_all(parent)
                .await
                .map_err(|error| SecretStoreError::Backend(Box::new(error)))?;
        }
        self.storage
            .write_atomic(&self.path, file.into())
            .await
            .map_err(|error| SecretStoreError::Backend(Box::new(error)))
    }

    async fn modify(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, String>) -> bool,
    ) -> Result<(), SecretStoreError> {
        let _writer = self.writer.lock().await;
        let mut entries = self.load().await?;
        if change(&mut entries.0) {
            self.store(&entries).await?;
        }
        Ok(())
    }
}

impl fmt::Debug for EncryptedFileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptedFileStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SecretStore for EncryptedFileStore {
    fn get<'a>(
        &'a self,
        key: &'a SecretKey,
    ) -> BoxFuture<'a, Result<Option<Secret>, SecretStoreError>> {
        Box::pin(async move {
            let entries = self.load().await?;
            Ok(entries.0.get(key.as_str()).map(Secret::new))
        })
    }

    fn put<'a>(
        &'a self,
        key: &'a SecretKey,
        secret: Secret,
    ) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async move {
            self.modify(|map| {
                if let Some(mut old) =
                    map.insert(key.as_str().to_owned(), secret.expose().to_owned())
                {
                    old.zeroize();
                }
                true
            })
            .await
        })
    }

    fn delete<'a>(&'a self, key: &'a SecretKey) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async move {
            self.modify(|map| {
                map.remove(key.as_str())
                    .map(|mut old| old.zeroize())
                    .is_some()
            })
            .await
        })
    }
}

/// Decrypted entries, wiped on drop.
#[derive(Default)]
struct Entries(BTreeMap<String, String>);

impl Drop for Entries {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}

#[cfg(test)]
#[path = "../tests/encrypted.rs"]
mod tests;
