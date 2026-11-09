use std::collections::HashMap;
use std::sync::Arc;

use coblet::auth::{LoginOptions, MICROSOFT_METHOD};
use coblet::player::SkinSource;
use coblet::runtime::{HttpUrl, Secret};
use coblet::types::{ExtensionId, PlayerName};

use crate::account::account_id;
use crate::java::extension_id;
use crate::{
    AccountInfo, AppearanceCapabilities, AppearanceServiceInfo, AuthMethodInfo, Coblet,
    CobletError, ErrorKind, GameHandle, LoginFlow, LoginInteraction, PlayerInfo, SkinModel,
};

fn invalid(error: &dyn std::fmt::Display) -> CobletError {
    CobletError::new(ErrorKind::InvalidArgument, error.to_string())
}

fn auth(error: coblet::auth::AuthError) -> CobletError {
    CobletError::from(coblet::Error::Auth(error))
}

#[uniffi::export(async_runtime = "tokio")]
impl Coblet {
    /// Lists the saved accounts without network access. Player profiles
    /// are snapshots; `refresh_profile` fetches the current one.
    ///
    /// # Errors
    ///
    /// Fails when `accounts.json` cannot be read.
    pub async fn accounts(&self) -> Result<Vec<AccountInfo>, CobletError> {
        let accounts = self.inner.accounts().list().await.map_err(auth)?;
        Ok(accounts.iter().map(AccountInfo::from).collect())
    }

    /// Returns one account without network access; see `accounts`.
    ///
    /// # Errors
    ///
    /// Fails with `NotFound` for an unknown ID.
    pub async fn account(&self, id: String) -> Result<AccountInfo, CobletError> {
        let account = self.inner.accounts().get(&account_id(&id)?).await;
        Ok(AccountInfo::from(&account.map_err(auth)?))
    }

    /// Returns the default account, the first one added unless changed.
    ///
    /// # Errors
    ///
    /// Fails when `accounts.json` cannot be read.
    pub async fn default_account(&self) -> Result<Option<AccountInfo>, CobletError> {
        let account = self
            .inner
            .accounts()
            .default_account()
            .await
            .map_err(auth)?;
        Ok(account.as_ref().map(AccountInfo::from))
    }

    /// Makes an account the default, or clears the default with `None`.
    ///
    /// # Errors
    ///
    /// Fails with `NotFound` for an unknown ID.
    pub async fn set_default_account(&self, id: Option<String>) -> Result<(), CobletError> {
        let id = id.as_deref().map(account_id).transpose()?;
        self.inner
            .accounts()
            .set_default(id.as_ref())
            .await
            .map_err(auth)
    }

    /// Chooses the account an instance launches with; `None` falls back to
    /// the default account.
    ///
    /// # Errors
    ///
    /// Fails with `NotFound` for an unknown account ID.
    pub async fn assign_account(
        &self,
        instance: String,
        account: Option<String>,
    ) -> Result<(), CobletError> {
        let account = account.as_deref().map(account_id).transpose()?;
        self.inner
            .accounts()
            .assign(&instance, account.as_ref())
            .await
            .map_err(auth)
    }

    /// Returns the account chosen for an instance, or the default account.
    ///
    /// # Errors
    ///
    /// Fails when `accounts.json` cannot be read.
    pub async fn account_for(&self, instance: String) -> Result<Option<AccountInfo>, CobletError> {
        let account = self
            .inner
            .accounts()
            .account_for(&instance)
            .await
            .map_err(auth)?;
        Ok(account.as_ref().map(AccountInfo::from))
    }

    /// Adds an offline account, or returns the one with that name.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidArgument` for a malformed player name.
    pub async fn add_offline_account(&self, name: String) -> Result<AccountInfo, CobletError> {
        let name = PlayerName::new(name).map_err(|error| invalid(&error))?;
        let account = self
            .inner
            .accounts()
            .add_offline(name)
            .await
            .map_err(auth)?;
        Ok(AccountInfo::from(&account))
    }

    /// Removes an account and its stored tokens.
    ///
    /// # Errors
    ///
    /// Fails with `NotFound` for an unknown ID.
    pub async fn remove_account(&self, id: String) -> Result<(), CobletError> {
        self.inner
            .accounts()
            .remove(&account_id(&id)?)
            .await
            .map_err(auth)
    }

    /// Renews the account's tokens now and returns its new state.
    ///
    /// # Errors
    ///
    /// Fails with `Auth` when the user has to sign in again and `Network`
    /// when a service cannot be reached.
    pub async fn refresh_account(&self, id: String) -> Result<AccountInfo, CobletError> {
        let account = self.inner.accounts().refresh(&account_id(&id)?).await;
        Ok(AccountInfo::from(&account.map_err(auth)?))
    }

    /// Checks with Minecraft services whether the account still owns the
    /// game, saves the result and returns the updated account; read
    /// `owns_game` and `license` on it.
    ///
    /// It reuses the current session, so it costs two requests unless the
    /// session needs renewing. An account that bought the game since its
    /// last check becomes `Ready`, one that lost it `NoLicense`. Offline
    /// accounts are returned unchanged, without a license.
    ///
    /// # Errors
    ///
    /// Fails with `Auth` when the user has to sign in again and `Network`
    /// when a service cannot be reached; the last license stays saved.
    pub async fn verify_license(&self, id: String) -> Result<AccountInfo, CobletError> {
        let account = self
            .inner
            .accounts()
            .verify_license(&account_id(&id)?)
            .await;
        Ok(AccountInfo::from(&account.map_err(auth)?))
    }

    /// Returns whether any saved account bought the game as of its last
    /// check, without network access. Prism Launcher allows offline
    /// accounts only when this holds; the policy is the launcher's.
    ///
    /// # Errors
    ///
    /// Fails when `accounts.json` cannot be read.
    pub async fn any_account_owns_game(&self) -> Result<bool, CobletError> {
        self.inner.accounts().any_owns_game().await.map_err(auth)
    }

    /// Imports the accounts of a Prism Launcher `accounts.json`. Microsoft
    /// accounts come in as `NeedsReauth`: their tokens are not copied.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or parsed.
    pub async fn import_prism_accounts(
        &self,
        path: String,
    ) -> Result<Vec<AccountInfo>, CobletError> {
        let accounts = self
            .inner
            .accounts()
            .import_prism(std::path::Path::new(&path))
            .await
            .map_err(auth)?;
        Ok(accounts.iter().map(AccountInfo::from).collect())
    }

    /// Lists the auth methods, such as `coblet:offline`,
    /// `coblet:microsoft` and those added with
    /// `CobletBuilder.add_auth_method`.
    #[must_use]
    pub fn auth_methods(&self) -> Vec<AuthMethodInfo> {
        self.inner
            .auth_methods()
            .iter()
            .map(|method| AuthMethodInfo {
                id: method.id().to_string(),
                name: method.display_name().to_owned(),
            })
            .collect()
    }

    /// Starts a login with any auth method. Show the flow's prompt, then
    /// call `LoginFlow.finish`. Signing in to an account already saved
    /// updates it instead of adding a second one.
    ///
    /// `params` and `secrets` are inputs the method defines, such as the
    /// address of a login server and a password; coblet's own methods
    /// ignore them.
    ///
    /// # Errors
    ///
    /// Fails with `NotFound` for an unknown method, `Unsupported` for
    /// Microsoft login without a Client ID, and with the method's own
    /// error when it cannot start.
    pub async fn begin_login(
        &self,
        method: String,
        interaction: LoginInteraction,
        login_hint: Option<String>,
        params: HashMap<String, String>,
        secrets: HashMap<String, String>,
    ) -> Result<Arc<LoginFlow>, CobletError> {
        let method = extension_id("auth method", &method)?;
        let mut options = LoginOptions::new().with_interaction(interaction.into());
        if let Some(hint) = login_hint {
            options = options.with_hint(hint);
        }
        for (name, value) in params {
            options = options.with_param(name, value);
        }
        for (name, value) in secrets {
            options = options.with_secret(name, Secret::new(value));
        }
        let flow = self.inner.begin_login(&method, &options).await?;
        Ok(Arc::new(LoginFlow::new(self.inner.clone(), method, flow)))
    }

    /// Starts a Microsoft login, like `begin_login` with
    /// `coblet:microsoft`.
    ///
    /// `login_hint` pre-fills the e-mail address on the sign-in page.
    ///
    /// # Errors
    ///
    /// Fails with `Unsupported` when no Client ID was set with
    /// `CobletBuilder.set_msa_client_id`, and `Network` when Microsoft
    /// cannot be reached.
    pub async fn begin_microsoft_login(
        &self,
        interaction: LoginInteraction,
        login_hint: Option<String>,
    ) -> Result<Arc<LoginFlow>, CobletError> {
        self.begin_login(
            MICROSOFT_METHOD.to_owned(),
            interaction,
            login_hint,
            HashMap::new(),
            HashMap::new(),
        )
        .await
    }

    /// Fetches the account's player profile (name, UUID, skins and capes)
    /// from Minecraft services now, saves it and returns the updated
    /// account.
    ///
    /// `accounts()` and `account()` return the snapshot saved at login,
    /// token renewal and skin or cape changes, so a change made on
    /// minecraft.net or in another launcher shows up only after this call.
    /// Show the snapshot at once and call this in the background. It sends
    /// one request with the current token; `refresh_account` renews the
    /// tokens themselves. Offline accounts are returned unchanged.
    ///
    /// # Errors
    ///
    /// Fails with `Auth` when the user has to sign in again, `NoProfile`
    /// for an account without a profile, and `Network` when the request
    /// fails.
    pub async fn refresh_profile(&self, id: String) -> Result<AccountInfo, CobletError> {
        let account = self.inner.refresh_profile(&account_id(&id)?).await?;
        Ok(AccountInfo::from(&account))
    }

    /// Downloads a skin or cape texture, such as `SkinInfo.url`, as PNG
    /// bytes, for example to draw the player's head.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidArgument` for a malformed URL and `Network` when
    /// the download fails or is larger than 256 KiB.
    pub async fn download_texture(&self, url: String) -> Result<Vec<u8>, CobletError> {
        let url = HttpUrl::new(url).map_err(|error| invalid(&error))?;
        Ok(self.inner.download_texture(&url).await?)
    }

    /// Lists the appearance services, such as
    /// `coblet:minecraft-services` and those added with
    /// `CobletBuilder.add_appearance_service`.
    #[must_use]
    pub fn appearance_services(&self) -> Vec<AppearanceServiceInfo> {
        self.inner
            .appearance_services()
            .iter()
            .map(|service| AppearanceServiceInfo {
                id: service.id().to_string(),
                name: service.display_name().to_owned(),
            })
            .collect()
    }

    /// Returns what the account's appearance service can change, so the
    /// launcher shows only the controls that work; all off for accounts
    /// without a service, such as offline ones.
    ///
    /// # Errors
    ///
    /// Fails with `NotFound` for an unknown account.
    pub async fn appearance_capabilities(
        &self,
        id: String,
    ) -> Result<AppearanceCapabilities, CobletError> {
        let caps = self
            .inner
            .appearance_capabilities(&account_id(&id)?)
            .await?;
        Ok(caps.into())
    }

    /// Uploads a PNG skin, 64x64 or 64x32, and wears it. Like the other
    /// skin and cape calls it returns the new player profile and saves it.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidArgument` for an image that is not a skin.
    pub async fn upload_skin(
        &self,
        id: String,
        png: Vec<u8>,
        model: SkinModel,
    ) -> Result<PlayerInfo, CobletError> {
        let profile = self
            .inner
            .upload_skin(&account_id(&id)?, SkinSource::Png(png), model.into())
            .await?;
        Ok(PlayerInfo::from(&profile))
    }

    /// Wears the skin at a public URL, which the account's appearance
    /// service downloads.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidArgument` for a malformed URL.
    pub async fn upload_skin_url(
        &self,
        id: String,
        url: String,
        model: SkinModel,
    ) -> Result<PlayerInfo, CobletError> {
        let url = HttpUrl::new(url).map_err(|error| invalid(&error))?;
        let profile = self
            .inner
            .upload_skin(&account_id(&id)?, SkinSource::Url(url), model.into())
            .await?;
        Ok(PlayerInfo::from(&profile))
    }

    /// Switches the arm width of the worn skin.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidState` when the player wears the default skin.
    pub async fn set_skin_model(
        &self,
        id: String,
        model: SkinModel,
    ) -> Result<PlayerInfo, CobletError> {
        let profile = self
            .inner
            .set_skin_model(&account_id(&id)?, model.into())
            .await?;
        Ok(PlayerInfo::from(&profile))
    }

    /// Goes back to the default skin.
    ///
    /// # Errors
    ///
    /// Fails when the request fails.
    pub async fn reset_skin(&self, id: String) -> Result<PlayerInfo, CobletError> {
        let profile = self.inner.reset_skin(&account_id(&id)?).await?;
        Ok(PlayerInfo::from(&profile))
    }

    /// Shows an owned cape by ID, or hides the cape with `None`.
    ///
    /// # Errors
    ///
    /// Fails with `InvalidArgument` for a cape the account does not own.
    pub async fn set_cape(
        &self,
        id: String,
        cape: Option<String>,
    ) -> Result<PlayerInfo, CobletError> {
        let profile = self
            .inner
            .set_cape(&account_id(&id)?, cape.as_deref())
            .await?;
        Ok(PlayerInfo::from(&profile))
    }

    /// Launches a version with an account, renewing its tokens first when
    /// needed. Without `game_dir` the game runs in
    /// `<data_root>/games/<version>`; `customizers` names registered
    /// customizers to run, in order. `operation_id`, from
    /// `generate_operation_id`, is the launch ID its events carry; a random
    /// one is used when absent.
    ///
    /// # Errors
    ///
    /// Fails with `NoLicense` for an account that does not own the game,
    /// `Auth` when the user has to sign in again, and like `launch_with`.
    #[uniffi::method(default(operation_id = None))]
    pub async fn launch_account(
        &self,
        version_id: String,
        account: String,
        game_dir: Option<String>,
        customizers: Vec<String>,
        operation_id: Option<String>,
    ) -> Result<Arc<GameHandle>, CobletError> {
        let mut request = self.inner.launch_request(&account_id(&account)?).await?;
        if let Some(id) = operation_id {
            request = request.with_operation(crate::events::operation_id(&id)?);
        }
        for id in customizers {
            request =
                request.with_customizer(ExtensionId::new(id).map_err(|error| invalid(&error))?);
        }
        let game_dir = game_dir.map_or_else(
            || self.inner.data_root().join("games").join(&version_id),
            Into::into,
        );
        let session = self.inner.launch(&version_id, game_dir, request).await?;
        Ok(Arc::new(GameHandle::new(session)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use coblet::auth::{
        AccountState, AccountUpdate, AuthFlow, LicenseCheck, PlayerProfile, callback_channel,
    };
    use coblet::runtime::{AuthPrompt, SecretKey, SecretStore as _};
    use coblet::types::{ExtensionId, PlayerName};

    use crate::secrets::ForeignSecrets;
    use crate::{
        AccountState as State, CobletBuilder, CobletError, ErrorKind, GameLicense, LoginFlow,
        LoginInteraction, LoginPrompt, SecretStore, generate_secret_key,
    };

    fn kind(error: &CobletError) -> ErrorKind {
        let CobletError::Failed { kind, .. } = error;
        *kind
    }

    fn builder(dir: &tempfile::TempDir) -> Arc<CobletBuilder> {
        CobletBuilder::new(
            dir.path().to_string_lossy().into_owned(),
            "coblet-uniffi-test/1.0".to_owned(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn offline_accounts_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(&dir).build().unwrap();
        let steve = coblet
            .add_offline_account("Steve".to_owned())
            .await
            .unwrap();
        assert_eq!(steve.method, "coblet:offline");
        assert_eq!(steve.state, State::Ready);
        assert_eq!(steve.player.as_ref().unwrap().name, "Steve");
        let alex = coblet.add_offline_account("Alex".to_owned()).await.unwrap();

        assert_eq!(coblet.accounts().await.unwrap().len(), 2);
        assert_eq!(
            coblet.default_account().await.unwrap().unwrap().id,
            steve.id
        );
        coblet
            .set_default_account(Some(alex.id.clone()))
            .await
            .unwrap();
        coblet
            .assign_account("pack".to_owned(), Some(steve.id.clone()))
            .await
            .unwrap();
        let chosen = coblet.account_for("pack".to_owned()).await.unwrap();
        assert_eq!(chosen.unwrap().id, steve.id);
        let other = coblet.account_for("other".to_owned()).await.unwrap();
        assert_eq!(other.unwrap().id, alex.id);

        coblet.remove_account(steve.id.clone()).await.unwrap();
        let missing = coblet.account(steve.id).await.unwrap_err();
        assert_eq!(kind(&missing), ErrorKind::NotFound);
        let bad = coblet.add_offline_account("no spaces".to_owned()).await;
        assert_eq!(kind(&bad.unwrap_err()), ErrorKind::InvalidArgument);
    }

    #[tokio::test]
    async fn offline_accounts_refresh_to_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(&dir).build().unwrap();
        let steve = coblet
            .add_offline_account("Steve".to_owned())
            .await
            .unwrap();
        let error = coblet.reset_skin(steve.id.clone()).await.unwrap_err();
        assert_eq!(kind(&error), ErrorKind::Unsupported);
        let unchanged = coblet.refresh_profile(steve.id.clone()).await.unwrap();
        assert_eq!(unchanged, steve);
        let unchecked = coblet.verify_license(steve.id.clone()).await.unwrap();
        assert_eq!(unchecked, steve);
        assert_eq!((steve.license, steve.owns_game), (None, false));
        assert!(!coblet.any_account_owns_game().await.unwrap());
        let bad = coblet.download_texture("not a url".to_owned()).await;
        assert_eq!(kind(&bad.unwrap_err()), ErrorKind::InvalidArgument);
    }

    #[tokio::test]
    async fn microsoft_login_needs_a_client_id() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(&dir).build().unwrap();
        let error = coblet
            .begin_microsoft_login(LoginInteraction::DeviceCode, None)
            .await
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::Unsupported);
        assert_eq!(
            kind(&builder(&dir).set_msa_client_id(String::new()).unwrap_err()),
            ErrorKind::InvalidArgument
        );
    }

    #[derive(Default)]
    struct Keychain(Mutex<HashMap<String, String>>);

    impl SecretStore for Keychain {
        fn get(&self, key: String) -> Result<Option<String>, CobletError> {
            Ok(self.0.lock().unwrap().get(&key).cloned())
        }

        fn put(&self, key: String, value: String) -> Result<(), CobletError> {
            if value.is_empty() {
                return Err(CobletError::new(ErrorKind::Other, "keychain is locked"));
            }
            self.0.lock().unwrap().insert(key, value);
            Ok(())
        }

        fn delete(&self, key: String) -> Result<(), CobletError> {
            self.0.lock().unwrap().remove(&key);
            Ok(())
        }
    }

    #[tokio::test]
    async fn foreign_secret_stores_back_coblet_secrets() {
        let keychain = Arc::new(Keychain::default());
        let store = ForeignSecrets(keychain.clone());
        let key = SecretKey::new("coblet:microsoft/a/refresh_token").unwrap();
        store
            .put(&key, coblet::runtime::Secret::new("token"))
            .await
            .unwrap();
        assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "token");
        assert_eq!(keychain.0.lock().unwrap().len(), 1);
        let error = store
            .put(&key, coblet::runtime::Secret::new(""))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("keychain is locked"), "{error}");
        store.delete(&key).await.unwrap();
        assert!(store.get(&key).await.unwrap().is_none());

        let dir = tempfile::tempdir().unwrap();
        let builder = builder(&dir);
        builder.set_secret_store(keychain).unwrap();
        builder.build().unwrap();
    }

    #[test]
    fn encrypted_secret_files_need_a_32_byte_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = generate_secret_key().unwrap();
        assert_eq!(key.len(), 32);
        let builder = builder(&dir);
        let error = builder
            .set_encrypted_secret_file("secrets.bin".to_owned(), vec![0; 16])
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        let path = dir.path().join("secrets.bin");
        builder
            .set_encrypted_secret_file(path.to_string_lossy().into_owned(), key)
            .unwrap();
        builder.build().unwrap();
    }

    fn facade(dir: &tempfile::TempDir) -> coblet::Coblet {
        coblet::Coblet::builder()
            .data_root(dir.path())
            .user_agent(coblet::runtime::UserAgent::new("coblet-uniffi-test/1.0").unwrap())
            .build()
            .unwrap()
    }

    fn offline() -> ExtensionId {
        ExtensionId::new("coblet:offline").unwrap()
    }

    fn notch() -> AccountUpdate {
        let profile = PlayerProfile::new(
            PlayerName::new("Notch").unwrap(),
            "069a79f444e94726a5befca90e38aaf5",
        )
        .unwrap();
        let mut update = AccountUpdate::new(AccountState::Ready).with_profile(profile);
        update.license = Some(LicenseCheck::new(
            coblet::auth::GameLicense::Purchased,
            std::time::SystemTime::UNIX_EPOCH,
        ));
        update
    }

    #[tokio::test]
    async fn login_flows_take_a_redirect_and_finish_once() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = callback_channel();
        let prompt = AuthPrompt::OpenUrl {
            url: coblet::runtime::HttpUrl::new("https://login.example/authorize").unwrap(),
            redirect_uri: "myapp://auth".to_owned(),
        };
        let flow = AuthFlow::new(prompt, async move {
            let uri = receiver.recv().await?;
            assert_eq!(uri, "myapp://auth?code=c&state=s");
            Ok(notch())
        })
        .with_callback(sender);
        let flow = LoginFlow::new(facade(&dir), offline(), flow);
        assert!(
            matches!(flow.prompt(), LoginPrompt::OpenUrl { ref redirect_uri, .. } if redirect_uri == "myapp://auth")
        );
        assert!(flow.accepts_redirect());
        flow.submit_redirect("myapp://auth?code=c&state=s".to_owned())
            .unwrap();
        assert!(!flow.accepts_redirect());
        assert_eq!(
            kind(
                &flow
                    .submit_redirect("myapp://again".to_owned())
                    .unwrap_err()
            ),
            ErrorKind::InvalidState
        );

        let account = flow.finish().await.unwrap();
        assert_eq!(account.license, Some(GameLicense::Purchased));
        assert_eq!(
            account.license_checked_at,
            Some(std::time::SystemTime::UNIX_EPOCH)
        );
        assert!(account.owns_game);
        assert_eq!(account.player.unwrap().name, "Notch");
        assert_eq!(
            kind(&flow.finish().await.unwrap_err()),
            ErrorKind::InvalidState
        );
    }

    #[tokio::test]
    async fn cancelled_login_flows_fail_with_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let flow = AuthFlow::new(AuthPrompt::Nothing, std::future::pending());
        let flow = Arc::new(LoginFlow::new(facade(&dir), offline(), flow));
        assert_eq!(
            kind(&flow.submit_redirect("x://y".to_owned()).unwrap_err()),
            ErrorKind::InvalidState
        );
        let waiting = tokio::spawn({
            let flow = Arc::clone(&flow);
            async move { flow.finish().await }
        });
        tokio::task::yield_now().await;
        flow.cancel();
        let error = waiting.await.unwrap().unwrap_err();
        assert_eq!(kind(&error), ErrorKind::Cancelled);
    }
}
