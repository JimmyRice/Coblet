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

    /// Puts an auth method in place of the one with the same ID, such as a
    /// patched `coblet:microsoft`, and returns the one it replaced. Saved
    /// accounts of that ID use the new method from then on.
    pub fn replace(&mut self, method: Arc<dyn AuthMethod>) -> Option<Arc<dyn AuthMethod>> {
        self.methods.replace(method)
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
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
    use coblet_runtime::{
        AuthPrompt, BoxFuture, Extension, MemorySecretStore, Request, Response, Secret, SecretKey,
        SecretStore, TokioStorage, Transport, TransportError,
    };
    use coblet_types::{ExtensionId, PlayerName};

    use super::AccountStore;
    use crate::{
        Account, AccountId, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError,
        AuthFlow, AuthMethod, GameLicense, LaunchSession, LicenseCheck, LoginOptions,
        PlayerProfile, RefreshMode, Refreshed,
    };

    fn checked(license: GameLicense) -> LicenseCheck {
        LicenseCheck::new(license, SystemTime::UNIX_EPOCH)
    }

    struct NoNetwork;

    impl Transport for NoNetwork {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(async { Err(TransportError::Timeout) })
        }
    }

    /// What the scripted method does on refresh.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        Ready,
        NoLicense,
        Revoked,
    }

    /// Method that counts token renewals and rotates `refresh_token` each time.
    struct Scripted {
        id: ExtensionId,
        renewals: AtomicUsize,
        outcome: Outcome,
    }

    impl Scripted {
        fn new(outcome: Outcome) -> Arc<Self> {
            Arc::new(Self {
                id: ExtensionId::new("test:scripted").unwrap(),
                renewals: AtomicUsize::new(0),
                outcome,
            })
        }
    }

    fn profile() -> PlayerProfile {
        PlayerProfile::new(
            PlayerName::new("Notch").unwrap(),
            "069a79f444e94726a5befca90e38aaf5",
        )
        .unwrap()
    }

    fn online(profile: &PlayerProfile, token: &str) -> LaunchSession {
        LaunchSession::new(LaunchAuth::online(OnlineSession::new(
            profile.name.clone(),
            profile.uuid.clone(),
            Credential::new(token),
        )))
    }

    impl Extension for Scripted {
        fn id(&self) -> &ExtensionId {
            &self.id
        }

        fn display_name(&self) -> &'static str {
            "Scripted"
        }
    }

    impl AuthMethod for Scripted {
        fn begin<'a>(
            &'a self,
            _ctx: &'a AuthCtx,
            _options: &'a LoginOptions,
        ) -> BoxFuture<'a, Result<AuthFlow, AuthError>> {
            Box::pin(async {
                let mut update = AccountUpdate::new(AccountState::Ready).with_profile(profile());
                update.secrets.insert("refresh_token", Secret::new("r0"));
                update.license = Some(checked(GameLicense::Purchased));
                update.session_expires_at = Some(SystemTime::now() - Duration::from_secs(1));
                Ok(AuthFlow::new(
                    AuthPrompt::Nothing,
                    async move { Ok(update) },
                ))
            })
        }

        fn refresh<'a>(
            &'a self,
            _ctx: &'a AuthCtx,
            account: &'a Account,
            secrets: &'a AccountSecrets,
            mode: RefreshMode,
        ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
            Box::pin(async move {
                let fresh = account
                    .session_expires_at
                    .is_some_and(|expires| expires > SystemTime::now());
                if mode == RefreshMode::IfNeeded && fresh {
                    let update = AccountUpdate::from_account(account);
                    let session = online(account.profile.as_ref().unwrap(), "cached");
                    return Ok(Refreshed::new(update, Some(session)));
                }
                let renewal = self.renewals.fetch_add(1, Ordering::SeqCst) + 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
                secrets
                    .put("refresh_token", Secret::new(format!("r{renewal}")))
                    .await?;
                match self.outcome {
                    Outcome::Revoked => Err(AuthError::InvalidGrant),
                    Outcome::NoLicense => {
                        let mut update = AccountUpdate::new(AccountState::NoLicense);
                        update.license = Some(checked(GameLicense::Missing));
                        Ok(Refreshed::new(update, None))
                    }
                    Outcome::Ready => {
                        let mut update =
                            AccountUpdate::new(AccountState::Ready).with_profile(profile());
                        update.session_expires_at =
                            Some(SystemTime::now() + Duration::from_secs(3600));
                        update.secrets.insert("access", Secret::new("a"));
                        Ok(Refreshed::new(update, Some(online(&profile(), "fresh"))))
                    }
                }
            })
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        secrets: Arc<MemorySecretStore>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                secrets: Arc::new(MemorySecretStore::new()),
            }
        }

        fn store(&self) -> AccountStore {
            AccountStore::new(
                Arc::new(TokioStorage::new()),
                self.dir.path(),
                self.secrets.clone(),
                AuthCtx::new(Arc::new(NoNetwork)),
            )
        }

        fn store_with(&self, method: Arc<Scripted>) -> AccountStore {
            let mut store = self.store();
            store.register(method).unwrap();
            store
        }

        async fn secret(&self, account: &AccountId, name: &str) -> Option<String> {
            let key = SecretKey::new(format!("test:scripted/{account}/{name}")).unwrap();
            self.secrets
                .get(&key)
                .await
                .unwrap()
                .map(|secret| secret.expose().to_owned())
        }
    }

    async fn log_in(store: &AccountStore) -> Account {
        let id = ExtensionId::new("test:scripted").unwrap();
        let flow = store.begin_login(&id, &LoginOptions::new()).await.unwrap();
        store.finish_login(&id, flow).await.unwrap()
    }

    #[tokio::test]
    async fn manages_offline_accounts() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert_eq!(store.list().await.unwrap().len(), 0);
        let steve = store
            .add_offline(PlayerName::new("Steve").unwrap())
            .await
            .unwrap();
        let again = store
            .add_offline(PlayerName::new("Steve").unwrap())
            .await
            .unwrap();
        assert_eq!(steve.id, again.id);
        let alex = store
            .add_offline(PlayerName::new("Alex").unwrap())
            .await
            .unwrap();
        assert_eq!(store.list().await.unwrap().len(), 2);
        assert_eq!(store.default_account().await.unwrap().unwrap().id, steve.id);

        store.assign("modded", Some(&alex.id)).await.unwrap();
        assert_eq!(
            store.account_for("modded").await.unwrap().unwrap().id,
            alex.id
        );
        assert_eq!(
            store.account_for("vanilla").await.unwrap().unwrap().id,
            steve.id
        );
        let session = store
            .session(&alex.id, RefreshMode::IfNeeded)
            .await
            .unwrap();
        assert!(matches!(session.auth, LaunchAuth::Offline(ref name) if name.as_str() == "Alex"));

        // A second store on the same data root sees the same file.
        let reopened = fixture.store();
        reopened.set_default(Some(&alex.id)).await.unwrap();
        assert_eq!(store.default_account().await.unwrap().unwrap().id, alex.id);

        store.remove(&alex.id).await.unwrap();
        assert!(store.default_account().await.unwrap().is_none());
        assert!(store.account_for("modded").await.unwrap().is_none());
        store.set_default(Some(&steve.id)).await.unwrap();
        assert_eq!(
            store.account_for("modded").await.unwrap().unwrap().id,
            steve.id
        );
        assert!(matches!(
            store.get(&alex.id).await,
            Err(AuthError::UnknownAccount(_))
        ));
        assert!(matches!(
            store.assign("modded", Some(&alex.id)).await,
            Err(AuthError::UnknownAccount(_))
        ));
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("\"format\": 1") && text.contains("Steve"));
    }

    #[tokio::test]
    async fn saves_login_secrets_and_relogin_updates_the_account() {
        let fixture = Fixture::new();
        let store = fixture.store_with(Scripted::new(Outcome::Ready));
        let account = log_in(&store).await;
        assert_eq!(account.state, AccountState::NeedsRefresh);
        assert!(account.secret_names.contains("refresh_token"));
        assert_eq!(
            fixture
                .secret(&account.id, "refresh_token")
                .await
                .as_deref(),
            Some("r0")
        );
        let again = log_in(&store).await;
        assert_eq!(again.id, account.id);
        assert_eq!(store.list().await.unwrap().len(), 1);
        assert!(
            !std::fs::read_to_string(store.path())
                .unwrap()
                .contains("r0")
        );

        store.remove(&account.id).await.unwrap();
        assert_eq!(fixture.secret(&account.id, "refresh_token").await, None);
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_one_renewal() {
        let fixture = Fixture::new();
        let method = Scripted::new(Outcome::Ready);
        let store = Arc::new(fixture.store_with(method.clone()));
        let account = log_in(&store).await;

        let calls: Vec<_> = (0..4)
            .map(|_| {
                let store = Arc::clone(&store);
                let id = account.id.clone();
                tokio::spawn(async move { store.session(&id, RefreshMode::Force).await })
            })
            .collect();
        for call in calls {
            call.await.unwrap().unwrap();
        }
        assert_eq!(method.renewals.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .secret(&account.id, "refresh_token")
                .await
                .as_deref(),
            Some("r1")
        );
        let saved = store.get(&account.id).await.unwrap();
        assert_eq!(saved.state, AccountState::Ready);
        assert!(saved.secret_names.contains("access"));

        // A later forced refresh renews again.
        store
            .session(&account.id, RefreshMode::Force)
            .await
            .unwrap();
        assert_eq!(method.renewals.load(Ordering::SeqCst), 2);
        let cached = store
            .session(&account.id, RefreshMode::IfNeeded)
            .await
            .unwrap();
        assert_eq!(cached.access_token().unwrap().expose(), "cached");
        assert_eq!(method.renewals.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn saves_what_a_failed_refresh_says_about_the_account() {
        let fixture = Fixture::new();
        let store = fixture.store_with(Scripted::new(Outcome::Revoked));
        let account = log_in(&store).await;
        assert!(matches!(
            store.session(&account.id, RefreshMode::IfNeeded).await,
            Err(AuthError::InvalidGrant)
        ));
        let saved = store.get(&account.id).await.unwrap();
        assert_eq!(saved.state, AccountState::NeedsReauth);
        assert_eq!(
            fixture
                .secret(&account.id, "refresh_token")
                .await
                .as_deref(),
            Some("r1")
        );

        let fixture = Fixture::new();
        let store = fixture.store_with(Scripted::new(Outcome::NoLicense));
        let account = log_in(&store).await;
        assert!(matches!(
            store.session(&account.id, RefreshMode::IfNeeded).await,
            Err(AuthError::NoLicense)
        ));
        let refreshed = store.refresh(&account.id).await.unwrap();
        assert_eq!(refreshed.state, AccountState::NoLicense);
        assert!(refreshed.profile.is_none());
    }

    #[tokio::test]
    async fn verifies_licenses_and_keeps_the_last_check() {
        let fixture = Fixture::new();
        let store = fixture.store_with(Scripted::new(Outcome::Revoked));
        let offline = store
            .add_offline(PlayerName::new("Steve").unwrap())
            .await
            .unwrap();
        assert!(!store.any_owns_game().await.unwrap());
        let account = log_in(&store).await;
        assert!(account.owns_game());
        assert!(store.any_owns_game().await.unwrap());
        // A failed check keeps the last license; only the state changes.
        assert!(matches!(
            store.verify_license(&account.id).await,
            Err(AuthError::InvalidGrant)
        ));
        let saved = store.get(&account.id).await.unwrap();
        assert_eq!(saved.state, AccountState::NeedsReauth);
        assert_eq!(saved.license, Some(checked(GameLicense::Purchased)));
        let unchanged = store.verify_license(&offline.id).await.unwrap();
        assert_eq!(unchanged.license, None);
        assert!(!unchanged.owns_game());

        let fixture = Fixture::new();
        let store = fixture.store_with(Scripted::new(Outcome::NoLicense));
        let account = log_in(&store).await;
        let checked_again = store.verify_license(&account.id).await.unwrap();
        assert_eq!(checked_again.license, Some(checked(GameLicense::Missing)));
        assert!(!store.any_owns_game().await.unwrap());
    }

    #[tokio::test]
    async fn rechecks_are_not_merged_into_other_refreshes() {
        let fixture = Fixture::new();
        let method = Scripted::new(Outcome::Ready);
        let store = Arc::new(fixture.store_with(method.clone()));
        let account = log_in(&store).await;
        let forced = tokio::spawn({
            let store = Arc::clone(&store);
            let id = account.id.clone();
            async move { store.refresh(&id).await }
        });
        tokio::task::yield_now().await;
        store.verify_license(&account.id).await.unwrap();
        forced.await.unwrap().unwrap();
        assert_eq!(method.renewals.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn reports_unknown_methods_and_broken_files() {
        let fixture = Fixture::new();
        let store = fixture.store_with(Scripted::new(Outcome::Ready));
        let account = log_in(&store).await;
        let without = fixture.store();
        assert!(matches!(
            without.session(&account.id, RefreshMode::IfNeeded).await,
            Err(AuthError::UnknownMethod(_))
        ));
        assert!(matches!(
            without
                .begin_login(
                    &ExtensionId::new("coblet:microsoft").unwrap(),
                    &LoginOptions::new()
                )
                .await,
            Err(AuthError::UnknownMethod(_))
        ));

        std::fs::write(store.path(), b"{\"format\": 9}").unwrap();
        assert!(matches!(
            store.list().await,
            Err(AuthError::AccountFile { .. })
        ));
    }

    #[tokio::test]
    async fn a_replaced_method_serves_the_saved_accounts() {
        let fixture = Fixture::new();
        let mut store = fixture.store_with(Scripted::new(Outcome::Ready));
        let update = AccountUpdate::new(AccountState::Ready);
        let account = store
            .add(&ExtensionId::new("test:scripted").unwrap(), update)
            .await
            .unwrap();
        let revoked = Scripted::new(Outcome::Revoked);
        let old = store.replace(revoked.clone()).unwrap();
        assert!(!Arc::ptr_eq(
            &old,
            &(revoked.clone() as Arc<dyn AuthMethod>)
        ));
        assert!(
            store
                .session(&account.id, RefreshMode::Force)
                .await
                .is_err()
        );
        assert_eq!(revoked.renewals.load(Ordering::SeqCst), 1);
        let offline = ExtensionId::new(crate::OFFLINE_METHOD).unwrap();
        assert!(
            store
                .replace(Arc::new(crate::OfflineMethod::new()))
                .is_some()
        );
        assert!(store.method(&offline).is_some());
    }

    #[tokio::test]
    async fn imports_prism_accounts_without_tokens() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let prism = fixture.dir.path().join("prism-accounts.json");
        std::fs::write(
        &prism,
        br#"{"formatVersion": 3, "accounts": [
            {"type": "MSA", "active": true, "profile": {"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch"},
             "msa": {"refresh_token": "never-copied"}},
            {"type": "Offline", "profile": {"id": "5627dd98e6be3c21b8a8e92344183641", "name": "Steve"}}
        ]}"#,
    )
    .unwrap();
        let added = store.import_prism(&prism).await.unwrap();
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].state, AccountState::NeedsReauth);
        assert_eq!(added[0].method.as_str(), "coblet:microsoft");
        assert!(added[0].secret_names.is_empty());
        assert_eq!(added[1].state, AccountState::Ready);
        assert_eq!(
            store.default_account().await.unwrap().unwrap().id,
            added[0].id
        );
        assert_eq!(store.import_prism(&prism).await.unwrap().len(), 0);
        assert!(
            !std::fs::read_to_string(store.path())
                .unwrap()
                .contains("never-copied")
        );
    }
}
