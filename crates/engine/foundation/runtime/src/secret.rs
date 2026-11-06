use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use crate::BoxFuture;

/// Name under which a secret is stored, such as `coblet:microsoft/3f2a/refresh_token`.
///
/// The key carries no secret material and may appear in logs.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SecretKey(String);

/// Reason a secret key was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretKeyError {
    /// The key is empty.
    #[error("secret key is empty")]
    Empty,
    /// The key contains whitespace or a control character.
    #[error("secret key {0:?} contains whitespace or a control character")]
    InvalidCharacter(String),
}

impl SecretKey {
    /// Validates a key.
    ///
    /// # Errors
    ///
    /// Returns [`SecretKeyError::Empty`] or [`SecretKeyError::InvalidCharacter`].
    pub fn new(key: impl Into<String>) -> Result<Self, SecretKeyError> {
        let key = key.into();
        if key.is_empty() {
            return Err(SecretKeyError::Empty);
        }
        if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(SecretKeyError::InvalidCharacter(key));
        }
        Ok(Self(key))
    }

    /// Returns the key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Sensitive value such as a refresh token. `Debug` and `Display` print `<redacted>`.
///
/// # Examples
///
/// ```
/// use coblet_runtime::Secret;
///
/// let token = Secret::new("M.C123_BAY.0.U");
/// assert_eq!(format!("{token:?}"), "<redacted>");
/// assert_eq!(token.expose(), "M.C123_BAY.0.U");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a sensitive value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the value in clear text.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Failure reported by a [`SecretStore`] backend.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    /// The backend cannot be reached, for example a locked keychain.
    #[error("secret store is unavailable: {0}")]
    Unavailable(String),
    /// The backend failed for another reason.
    #[error("secret store failed: {0}")]
    Backend(#[source] Box<dyn Error + Send + Sync>),
}

/// Storage for credentials such as refresh and access tokens.
///
/// coblet keeps non-sensitive account data in its own files and every secret
/// in a `SecretStore`. Platform keychains (macOS Keychain, Windows Credential
/// Manager, Secret Service) are implemented outside the engine.
pub trait SecretStore: Send + Sync + 'static {
    /// Reads a secret, returning `None` when the key has no value.
    fn get<'a>(
        &'a self,
        key: &'a SecretKey,
    ) -> BoxFuture<'a, Result<Option<Secret>, SecretStoreError>>;

    /// Stores a secret, replacing any previous value.
    fn put<'a>(
        &'a self,
        key: &'a SecretKey,
        secret: Secret,
    ) -> BoxFuture<'a, Result<(), SecretStoreError>>;

    /// Removes a secret. Removing a missing key succeeds.
    fn delete<'a>(&'a self, key: &'a SecretKey) -> BoxFuture<'a, Result<(), SecretStoreError>>;
}

/// Secret store that keeps values in process memory and loses them on exit.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    entries: Mutex<HashMap<SecretKey, Secret>>,
}

impl MemorySecretStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with_entries<T>(&self, f: impl FnOnce(&mut HashMap<SecretKey, Secret>) -> T) -> T {
        // A panic in another thread cannot leave a HashMap half-updated in a way
        // that matters here, so a poisoned lock is still safe to use.
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut entries)
    }
}

impl SecretStore for MemorySecretStore {
    fn get<'a>(
        &'a self,
        key: &'a SecretKey,
    ) -> BoxFuture<'a, Result<Option<Secret>, SecretStoreError>> {
        let value = self.with_entries(|entries| entries.get(key).cloned());
        Box::pin(async move { Ok(value) })
    }

    fn put<'a>(
        &'a self,
        key: &'a SecretKey,
        secret: Secret,
    ) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        self.with_entries(|entries| entries.insert(key.clone(), secret));
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(&'a self, key: &'a SecretKey) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        self.with_entries(|entries| entries.remove(key));
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::{MemorySecretStore, Secret, SecretKey, SecretKeyError, SecretStore};

    #[tokio::test]
    async fn stores_replaces_and_deletes() {
        let store: Box<dyn SecretStore> = Box::new(MemorySecretStore::new());
        let key = SecretKey::new("coblet:microsoft/abc/refresh_token").unwrap();
        assert_eq!(store.get(&key).await.unwrap(), None);
        store.put(&key, Secret::new("first")).await.unwrap();
        store.put(&key, Secret::new("second")).await.unwrap();
        assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "second");
        store.delete(&key).await.unwrap();
        store.delete(&key).await.unwrap();
        assert_eq!(store.get(&key).await.unwrap(), None);
    }

    #[test]
    fn never_prints_secret_values() {
        let secret = Secret::new("hunter2");
        assert_eq!(secret.to_string(), "<redacted>");
        assert!(!format!("{:?}", Some(secret)).contains("hunter2"));
    }

    #[test]
    fn validates_keys() {
        assert_eq!(SecretKey::new(""), Err(SecretKeyError::Empty));
        assert!(matches!(
            SecretKey::new("a b"),
            Err(SecretKeyError::InvalidCharacter(_))
        ));
    }
}
