use std::sync::Arc;

use coblet::auth::{LoginOptions, MICROSOFT_METHOD};
use coblet::player::SkinSource;
use coblet::runtime::HttpUrl;
use coblet::types::{ExtensionId, PlayerName};

use crate::account::account_id;
use crate::{
    AccountInfo, Coblet, CobletError, ErrorKind, GameHandle, LoginFlow, LoginInteraction,
    PlayerInfo, SkinModel,
};

fn invalid(error: &dyn std::fmt::Display) -> CobletError {
    CobletError::new(ErrorKind::InvalidArgument, error.to_string())
}

fn auth(error: coblet::auth::AuthError) -> CobletError {
    CobletError::from(coblet::Error::Auth(error))
}

#[uniffi::export(async_runtime = "tokio")]
impl Coblet {
    /// Lists the saved accounts.
    ///
    /// # Errors
    ///
    /// Fails when `accounts.json` cannot be read.
    pub async fn accounts(&self) -> Result<Vec<AccountInfo>, CobletError> {
        let accounts = self.inner.accounts().list().await.map_err(auth)?;
        Ok(accounts.iter().map(AccountInfo::from).collect())
    }

    /// Returns one account.
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

    /// Starts a Microsoft login. Signing in to an account already saved
    /// updates it instead of adding a second one.
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
        let mut options = LoginOptions::new().with_interaction(interaction.into());
        if let Some(hint) = login_hint {
            options = options.with_hint(hint);
        }
        let method = ExtensionId::new(MICROSOFT_METHOD).map_err(|error| invalid(&error))?;
        let flow = self.inner.begin_login(&method, &options).await?;
        Ok(Arc::new(LoginFlow::new(self.inner.clone(), method, flow)))
    }

    /// Reads the account's skins and capes from Minecraft services and
    /// saves them on the account.
    ///
    /// # Errors
    ///
    /// Fails with `Unsupported` for offline accounts.
    pub async fn appearance(&self, id: String) -> Result<PlayerInfo, CobletError> {
        let profile = self.inner.appearance(&account_id(&id)?).await?;
        Ok(PlayerInfo::from(&profile))
    }

    /// Uploads a PNG skin, 64x64 or 64x32, and wears it.
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

    /// Wears the skin at a public URL, which Minecraft services download.
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
    /// customizers to run, in order.
    ///
    /// # Errors
    ///
    /// Fails with `NoLicense` for an account that does not own the game,
    /// `Auth` when the user has to sign in again, and like `launch_with`.
    pub async fn launch_account(
        &self,
        version_id: String,
        account: String,
        game_dir: Option<String>,
        customizers: Vec<String>,
    ) -> Result<Arc<GameHandle>, CobletError> {
        let mut request = self.inner.launch_request(&account_id(&account)?).await?;
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
#[path = "accounts_tests.rs"]
mod tests;
