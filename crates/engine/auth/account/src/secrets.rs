use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use coblet_runtime::{Secret, SecretKey, SecretStore};
use coblet_types::ExtensionId;

use crate::{AccountId, AuthError};

/// Secrets of one account, stored under `<method>/<account>/<name>`.
///
/// Writes go to the [`SecretStore`] at once, so a refresh token that
/// rotates is saved even when a later step of the refresh fails.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_auth::{AccountId, AccountSecrets};
/// use coblet_runtime::{MemorySecretStore, Secret};
/// use coblet_types::ExtensionId;
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let secrets = AccountSecrets::new(
///     Arc::new(MemorySecretStore::new()),
///     &ExtensionId::new("coblet:microsoft")?,
///     &AccountId::new("main")?,
/// );
/// secrets.put("refresh_token", Secret::new("M.C1")).await?;
/// assert_eq!(secrets.key("refresh_token")?.as_str(), "coblet:microsoft/main/refresh_token");
/// assert!(secrets.written().contains("refresh_token"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// ```
pub struct AccountSecrets {
    store: Arc<dyn SecretStore>,
    prefix: String,
    written: Mutex<BTreeSet<String>>,
}

impl AccountSecrets {
    /// Scopes `store` to one account.
    #[must_use]
    pub fn new(store: Arc<dyn SecretStore>, method: &ExtensionId, account: &AccountId) -> Self {
        Self {
            store,
            prefix: format!("{method}/{account}/"),
            written: Mutex::new(BTreeSet::new()),
        }
    }

    /// Returns the store key of a secret name.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::SecretKey`] when the name contains whitespace or
    /// a control character.
    pub fn key(&self, name: &str) -> Result<SecretKey, AuthError> {
        Ok(SecretKey::new(format!("{}{name}", self.prefix))?)
    }

    /// Reads a secret.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Secrets`] when the store fails.
    pub async fn get(&self, name: &str) -> Result<Option<Secret>, AuthError> {
        let key = self.key(name)?;
        Ok(self.store.get(&key).await?)
    }

    /// Saves a secret, replacing the previous value.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Secrets`] when the store fails.
    pub async fn put(&self, name: &str, secret: Secret) -> Result<(), AuthError> {
        let key = self.key(name)?;
        self.store.put(&key, secret).await?;
        self.lock().insert(name.to_owned());
        Ok(())
    }

    /// Removes a secret.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Secrets`] when the store fails.
    pub async fn delete(&self, name: &str) -> Result<(), AuthError> {
        let key = self.key(name)?;
        self.store.delete(&key).await?;
        Ok(())
    }

    /// Returns the names written through this value.
    #[must_use]
    pub fn written(&self) -> BTreeSet<String> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.written.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for AccountSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountSecrets")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}
