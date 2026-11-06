use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use coblet_runtime::{LockGuard, Registry, RegistryError, SecretStore, Storage};
use coblet_types::{ExtensionId, PlayerName};

use super::file::AccountsFile;
use super::prism;
use crate::{
    Account, AccountId, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError, AuthFlow,
    AuthMethod, LaunchSession, LoginOptions, OFFLINE_METHOD, OfflineMethod, PlayerProfile,
    RefreshMode,
};

/// Name of the account file inside the data root.
pub const ACCOUNTS_FILE: &str = "accounts.json";

/// Accounts of one launcher: logins, refreshes, the default account and
/// the account chosen for each instance.
///
/// Non-sensitive data is kept in `data_root/accounts.json`, tokens in the
/// [`SecretStore`]. Every change re-reads the file under a file lock, so
/// two processes sharing a data root do not overwrite each other.
///
/// Concurrent [`session`](Self::session) calls for one account share a
/// single refresh: later callers wait and reuse its result.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_auth::{AccountStore, AuthCtx};
/// use coblet_runtime::{MemorySecretStore, TokioStorage};
/// use coblet_types::PlayerName;
/// # use coblet_runtime::{BoxFuture, Request, Response, Transport, TransportError};
/// # struct NoNetwork;
/// # impl Transport for NoNetwork {
/// #     fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
/// #         Box::pin(async { Err(TransportError::Timeout) })
/// #     }
/// # }
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// # let dir = tempfile::tempdir()?;
/// let store = AccountStore::new(
///     Arc::new(TokioStorage::new()),
///     dir.path(),
///     Arc::new(MemorySecretStore::new()),
///     AuthCtx::new(Arc::new(NoNetwork)),
/// );
/// let steve = store.add_offline(PlayerName::new("Steve")?).await?;
/// assert_eq!(store.default_account().await?.unwrap().id, steve.id);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// ```
pub struct AccountStore {
    storage: Arc<dyn Storage>,
    path: PathBuf,
    lock_path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    methods: Registry<dyn AuthMethod>,
    ctx: AuthCtx,
    writer: tokio::sync::Mutex<()>,
    slots: Mutex<HashMap<AccountId, Arc<RefreshSlot>>>,
}

/// Serializes refreshes of one account.
#[derive(Default)]
struct RefreshSlot {
    lock: tokio::sync::Mutex<()>,
    generation: AtomicU64,
}

/// The account file, loaded under the writer and file locks.
struct Locked<'a> {
    store: &'a AccountStore,
    file: AccountsFile,
    _writer: tokio::sync::MutexGuard<'a, ()>,
    _lock: Box<dyn LockGuard>,
}

impl Locked<'_> {
    async fn save(self) -> Result<(), AuthError> {
        let store = self.store;
        if let Some(parent) = store.path.parent() {
            store.storage.create_dir_all(parent).await?;
        }
        store
            .storage
            .write_atomic(&store.path, self.file.to_json().into())
            .await?;
        Ok(())
    }
}

impl AccountStore {
    /// Creates a store for `data_root/accounts.json` with the offline method registered.
    #[must_use]
    pub fn new(
        storage: Arc<dyn Storage>,
        data_root: &Path,
        secrets: Arc<dyn SecretStore>,
        ctx: AuthCtx,
    ) -> Self {
        let mut methods: Registry<dyn AuthMethod> = Registry::new();
        // The registry is empty, so the first registration cannot collide.
        let _ = methods.register(Arc::new(OfflineMethod::new()));
        Self {
            storage,
            path: data_root.join(ACCOUNTS_FILE),
            lock_path: data_root.join("accounts.json.lock"),
            secrets,
            methods,
            ctx,
            writer: tokio::sync::Mutex::new(()),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Adds an auth method, such as Microsoft login or a third-party one.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when the ID is taken.
    pub fn register(&mut self, method: Arc<dyn AuthMethod>) -> Result<(), RegistryError> {
        self.methods.register(method)
    }

    /// Returns a registered method.
    #[must_use]
    pub fn method(&self, id: &ExtensionId) -> Option<&Arc<dyn AuthMethod>> {
        self.methods.get(id)
    }

    /// Returns the registered methods in ID order.
    #[must_use]
    pub fn methods(&self) -> &Registry<dyn AuthMethod> {
        &self.methods
    }

    /// Returns the services handed to auth methods.
    #[must_use]
    pub fn ctx(&self) -> &AuthCtx {
        &self.ctx
    }

    /// Returns the path of the account file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the secret store.
    #[must_use]
    pub fn secret_store(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// Lists the accounts in the order they were added.
    ///
    /// A ready account whose session has expired is reported as
    /// [`AccountState::NeedsRefresh`].
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Storage`] or [`AuthError::AccountFile`] when the
    /// file cannot be read.
    pub async fn list(&self) -> Result<Vec<Account>, AuthError> {
        let file = self.read().await?;
        Ok(file.accounts.into_iter().map(|a| self.view(a)).collect())
    }

    /// Returns one account.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::UnknownAccount`] when no account has this ID.
    pub async fn get(&self, id: &AccountId) -> Result<Account, AuthError> {
        let account = self.raw(id).await?;
        Ok(self.view(account))
    }

    /// Returns the default account, if one is set.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read.
    pub async fn default_account(&self) -> Result<Option<Account>, AuthError> {
        let file = self.read().await?;
        Ok(file
            .default
            .as_ref()
            .and_then(|id| file.find(id).cloned())
            .map(|account| self.view(account)))
    }

    /// Sets or clears the default account.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::UnknownAccount`] for an unknown ID.
    pub async fn set_default(&self, id: Option<&AccountId>) -> Result<(), AuthError> {
        let mut locked = self.lock().await?;
        if let Some(id) = id {
            locked
                .file
                .find(id)
                .ok_or_else(|| AuthError::UnknownAccount(id.clone()))?;
        }
        locked.file.default = id.cloned();
        locked.save().await
    }

    /// Chooses the account an instance launches with; `None` falls back to
    /// the default account.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::UnknownAccount`] for an unknown ID.
    pub async fn assign(
        &self,
        instance: &str,
        account: Option<&AccountId>,
    ) -> Result<(), AuthError> {
        let mut locked = self.lock().await?;
        match account {
            Some(id) => {
                locked
                    .file
                    .find(id)
                    .ok_or_else(|| AuthError::UnknownAccount(id.clone()))?;
                locked
                    .file
                    .instances
                    .insert(instance.to_owned(), id.clone());
            }
            None => {
                locked.file.instances.remove(instance);
            }
        }
        locked.save().await
    }

    /// Returns the account chosen for an instance, or the default account.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read.
    pub async fn account_for(&self, instance: &str) -> Result<Option<Account>, AuthError> {
        let file = self.read().await?;
        let id = file.instances.get(instance).or(file.default.as_ref());
        Ok(id
            .and_then(|id| file.find(id).cloned())
            .map(|account| self.view(account)))
    }

    /// Starts a login with a registered method.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::UnknownMethod`] or the error of the method.
    pub async fn begin_login(
        &self,
        method: &ExtensionId,
        options: &LoginOptions,
    ) -> Result<AuthFlow, AuthError> {
        self.require(method)?.begin(&self.ctx, options).await
    }

    /// Waits for a login started with [`begin_login`](Self::begin_login)
    /// and saves the account.
    ///
    /// # Errors
    ///
    /// Returns the error of the flow or of saving.
    pub async fn finish_login(
        &self,
        method: &ExtensionId,
        flow: AuthFlow,
    ) -> Result<Account, AuthError> {
        let update = flow.finish().await?;
        self.add(method, update).await
    }

    /// Saves the result of a login.
    ///
    /// Signing in again with an account already in the store (same profile
    /// UUID, or same Xbox user for accounts without a profile) updates that
    /// account instead of adding a second one. The first account becomes
    /// the default.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::UnknownMethod`], or an error from the secret
    /// store or the file.
    pub async fn add(
        &self,
        method: &ExtensionId,
        update: AccountUpdate,
    ) -> Result<Account, AuthError> {
        self.require(method)?;
        let mut locked = self.lock().await?;
        let existing = locked.file.find_same(method, &update).cloned();
        let mut account =
            existing.unwrap_or_else(|| Account::new(AccountId::generate(), method.clone()));
        let secrets = AccountSecrets::new(Arc::clone(&self.secrets), method, &account.id);
        for (name, secret) in update.secrets.iter() {
            secrets.put(name, secret.clone()).await?;
        }
        merge(&mut account, update, &secrets);
        match locked.file.find_mut(&account.id) {
            Some(slot) => *slot = account.clone(),
            None => locked.file.accounts.push(account.clone()),
        }
        if locked.file.default.is_none() {
            locked.file.default = Some(account.id.clone());
        }
        locked.save().await?;
        Ok(self.view(account))
    }

    /// Adds an offline account, or returns the existing one with this name.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be written.
    pub async fn add_offline(&self, name: PlayerName) -> Result<Account, AuthError> {
        let method = ExtensionId::new(OFFLINE_METHOD).unwrap_or_else(|_| unreachable!());
        self.add(&method, OfflineMethod::update_for(name)).await
    }

    /// Removes an account with its secrets, its default flag and its
    /// instance assignments.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::UnknownAccount`] for an unknown ID, or an error
    /// from the secret store or the file.
    pub async fn remove(&self, id: &AccountId) -> Result<(), AuthError> {
        let mut locked = self.lock().await?;
        let account = locked
            .file
            .find(id)
            .cloned()
            .ok_or_else(|| AuthError::UnknownAccount(id.clone()))?;
        let secrets = AccountSecrets::new(Arc::clone(&self.secrets), &account.method, id);
        for name in &account.secret_names {
            secrets.delete(name).await?;
        }
        locked.file.accounts.retain(|account| &account.id != id);
        locked.file.instances.retain(|_, account| account != id);
        if locked.file.default.as_ref() == Some(id) {
            locked.file.default = None;
        }
        locked.save().await?;
        self.slots_lock().remove(id);
        Ok(())
    }

    /// Replaces the profile snapshot, for example after a skin change.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::UnknownAccount`] for an unknown ID.
    pub async fn set_profile(
        &self,
        id: &AccountId,
        profile: PlayerProfile,
    ) -> Result<Account, AuthError> {
        let mut locked = self.lock().await?;
        let account = locked
            .file
            .find_mut(id)
            .ok_or_else(|| AuthError::UnknownAccount(id.clone()))?;
        account.profile = Some(profile);
        let account = account.clone();
        locked.save().await?;
        Ok(self.view(account))
    }

    /// Returns a session to launch with, renewing tokens when they are
    /// about to expire or `mode` is [`RefreshMode::Force`].
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::NoLicense`] or [`AuthError::NoProfile`] when the
    /// account cannot play, and the error of the refresh otherwise. Errors
    /// that say something about the account, such as
    /// [`AuthError::InvalidGrant`], are saved as its state.
    pub async fn session(
        &self,
        id: &AccountId,
        mode: RefreshMode,
    ) -> Result<LaunchSession, AuthError> {
        let (account, session) = self.run_refresh(id, mode).await?;
        session.ok_or(match account.state {
            AccountState::NoLicense => AuthError::NoLicense,
            _ => AuthError::NoProfile,
        })
    }

    /// Renews the account now and returns its updated data, also when it
    /// cannot play.
    ///
    /// # Errors
    ///
    /// Returns the error of the refresh.
    pub async fn refresh(&self, id: &AccountId) -> Result<Account, AuthError> {
        Ok(self.run_refresh(id, RefreshMode::Force).await?.0)
    }

    /// Reads the account's license and profile from the services again,
    /// saves them and returns the updated account.
    ///
    /// The current session is reused while it is valid, so this costs two
    /// requests; an expiring session is renewed first. A Microsoft account
    /// that bought the game since its last check becomes ready, and one
    /// that lost it becomes [`AccountState::NoLicense`]. Accounts of methods
    /// without licenses, such as offline ones, are returned with
    /// [`Account::license`] still `None`.
    ///
    /// # Errors
    ///
    /// Returns the error of the refresh, for example
    /// [`AuthError::InvalidGrant`] when the user has to sign in again.
    pub async fn verify_license(&self, id: &AccountId) -> Result<Account, AuthError> {
        Ok(self.run_refresh(id, RefreshMode::Recheck).await?.0)
    }

    /// Returns whether any saved account bought the game as of its last
    /// license check, without network access.
    ///
    /// Prism Launcher uses the same test to allow offline accounts only on
    /// installs that also hold a bought account; coblet leaves that policy
    /// to the launcher.
    ///
    /// # Errors
    ///
    /// Returns an error when `accounts.json` cannot be read.
    pub async fn any_owns_game(&self) -> Result<bool, AuthError> {
        Ok(self.list().await?.iter().any(Account::owns_game))
    }

    /// Imports the accounts of a Prism Launcher `accounts.json` without
    /// their tokens.
    ///
    /// Microsoft accounts are added as [`AccountState::NeedsReauth`] and
    /// need a new login; offline accounts are ready. Accounts already in the
    /// store are skipped. Prism's active account becomes the default when
    /// no default is set.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::AccountFile`] when the file is not a Prism
    /// account list, or an error from the file.
    pub async fn import_prism(&self, path: &Path) -> Result<Vec<Account>, AuthError> {
        let bytes = self.storage.read(path).await?;
        let entries = prism::parse(&bytes).map_err(|message| AuthError::AccountFile {
            path: path.display().to_string(),
            message,
        })?;
        let mut locked = self.lock().await?;
        let mut added = Vec::new();
        for entry in entries {
            let method = if entry.offline {
                OFFLINE_METHOD
            } else {
                "coblet:microsoft"
            };
            let method = ExtensionId::new(method).unwrap_or_else(|_| unreachable!());
            if locked
                .file
                .find_player(&method, &entry.profile.uuid)
                .is_some()
            {
                continue;
            }
            let mut account = Account::new(AccountId::generate(), method);
            account.state = if entry.offline {
                AccountState::Ready
            } else {
                AccountState::NeedsReauth
            };
            account.profile = Some(entry.profile);
            if entry.active && locked.file.default.is_none() {
                locked.file.default = Some(account.id.clone());
            }
            locked.file.accounts.push(account.clone());
            added.push(self.view(account));
        }
        locked.save().await?;
        Ok(added)
    }

    async fn run_refresh(
        &self,
        id: &AccountId,
        mode: RefreshMode,
    ) -> Result<(Account, Option<LaunchSession>), AuthError> {
        let slot = self.slot(id);
        let seen = slot.generation.load(Ordering::Acquire);
        let _guard = slot.lock.lock().await;
        // A refresh finished while this call waited: reuse it instead of
        // forcing another. A recheck still runs; the other refresh may have
        // reused the session without reading the license.
        let mode = if mode == RefreshMode::Force && slot.generation.load(Ordering::Acquire) != seen
        {
            RefreshMode::IfNeeded
        } else {
            mode
        };
        let account = self.raw(id).await?;
        let method = Arc::clone(self.require(&account.method)?);
        let secrets = AccountSecrets::new(Arc::clone(&self.secrets), &account.method, id);
        let result = method.refresh(&self.ctx, &account, &secrets, mode).await;
        slot.generation.fetch_add(1, Ordering::AcqRel);
        match result {
            Ok(refreshed) => {
                let saved = self.save_refresh(id, refreshed.update, &secrets).await?;
                Ok((saved, refreshed.session))
            }
            Err(error) => {
                self.save_failure(id, error.implied_state(), &secrets)
                    .await?;
                Err(error)
            }
        }
    }

    async fn save_refresh(
        &self,
        id: &AccountId,
        update: AccountUpdate,
        secrets: &AccountSecrets,
    ) -> Result<Account, AuthError> {
        for (name, secret) in update.secrets.iter() {
            secrets.put(name, secret.clone()).await?;
        }
        let mut locked = self.lock().await?;
        let account = locked
            .file
            .find_mut(id)
            .ok_or_else(|| AuthError::UnknownAccount(id.clone()))?;
        merge(account, update, secrets);
        let account = account.clone();
        locked.save().await?;
        Ok(self.view(account))
    }

    async fn save_failure(
        &self,
        id: &AccountId,
        state: Option<AccountState>,
        secrets: &AccountSecrets,
    ) -> Result<(), AuthError> {
        let written = secrets.written();
        if state.is_none() && written.is_empty() {
            return Ok(());
        }
        let mut locked = self.lock().await?;
        let Some(account) = locked.file.find_mut(id) else {
            return Ok(());
        };
        if let Some(state) = state {
            account.state = state;
        }
        account.secret_names.extend(written);
        locked.save().await
    }

    fn require(&self, method: &ExtensionId) -> Result<&Arc<dyn AuthMethod>, AuthError> {
        self.methods
            .get(method)
            .ok_or_else(|| AuthError::UnknownMethod(method.clone()))
    }

    fn view(&self, mut account: Account) -> Account {
        account.state = account.state_at(self.ctx.now());
        account
    }

    async fn raw(&self, id: &AccountId) -> Result<Account, AuthError> {
        self.read()
            .await?
            .find(id)
            .cloned()
            .ok_or_else(|| AuthError::UnknownAccount(id.clone()))
    }

    async fn read(&self) -> Result<AccountsFile, AuthError> {
        match self.storage.read(&self.path).await {
            Ok(bytes) => AccountsFile::parse(&bytes).map_err(|message| AuthError::AccountFile {
                path: self.path.display().to_string(),
                message,
            }),
            Err(error) if error.is_not_found() => Ok(AccountsFile::default()),
            Err(error) => Err(error.into()),
        }
    }

    async fn lock(&self) -> Result<Locked<'_>, AuthError> {
        let writer = self.writer.lock().await;
        if let Some(parent) = self.lock_path.parent() {
            self.storage.create_dir_all(parent).await?;
        }
        let lock = self.storage.lock(&self.lock_path).await?;
        let file = self.read().await?;
        Ok(Locked {
            store: self,
            file,
            _writer: writer,
            _lock: lock,
        })
    }

    fn slot(&self, id: &AccountId) -> Arc<RefreshSlot> {
        Arc::clone(self.slots_lock().entry(id.clone()).or_default())
    }

    fn slots_lock(&self) -> std::sync::MutexGuard<'_, HashMap<AccountId, Arc<RefreshSlot>>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for AccountStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountStore")
            .field("path", &self.path)
            .field(
                "methods",
                &self
                    .methods
                    .iter()
                    .map(|method| method.id().to_string())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Applies an update to an account and records the secret names.
fn merge(account: &mut Account, update: AccountUpdate, secrets: &AccountSecrets) {
    account.secret_names.extend(secrets.written());
    account
        .secret_names
        .extend(update.secrets.iter().map(|(name, _)| name.to_owned()));
    account.profile = update.profile;
    account.state = update.state;
    if update.license.is_some() {
        account.license = update.license;
    }
    account.session_expires_at = update.session_expires_at;
    account.data = update.data;
}

#[cfg(test)]
#[path = "../../tests/store/accounts.rs"]
mod tests;
