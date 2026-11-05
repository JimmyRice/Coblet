use std::future::Future;

use coblet_auth::{AccountId, PlayerProfile, RefreshMode, SkinModel};
use coblet_launch_command::Credential;
use coblet_player::{Player, PlayerError, SkinLibrary, SkinSource};

use crate::accounts::auth_error;
use crate::{Coblet, Error};

impl Coblet {
    /// Returns the local skin library in `data_root/skins/`.
    #[must_use]
    pub fn skins(&self) -> &SkinLibrary {
        &self.inner.skins
    }

    /// Returns the account's current skins and capes from Minecraft services.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] with [`PlayerError::Offline`] for offline
    /// accounts, and the error of the request otherwise.
    pub async fn appearance(&self, account: &AccountId) -> Result<PlayerProfile, Error> {
        self.with_token(account, |player, token| async move {
            player.appearance(&token).await
        })
        .await
    }

    /// Uploads a skin for the account and makes it active.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] for an invalid image, an offline account or
    /// a failed request.
    pub async fn upload_skin(
        &self,
        account: &AccountId,
        source: SkinSource,
        model: SkinModel,
    ) -> Result<PlayerProfile, Error> {
        self.with_token(account, |player, token| {
            let source = source.clone();
            async move { player.upload_skin(&token, source, model).await }
        })
        .await
    }

    /// Switches the arm model of the active skin.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] with [`PlayerError::NoSkin`] when the
    /// player wears the default skin.
    pub async fn set_skin_model(
        &self,
        account: &AccountId,
        model: SkinModel,
    ) -> Result<PlayerProfile, Error> {
        self.with_token(account, |player, token| async move {
            player.set_model(&token, model).await
        })
        .await
    }

    /// Goes back to the default skin.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] when the request fails.
    pub async fn reset_skin(&self, account: &AccountId) -> Result<PlayerProfile, Error> {
        self.with_token(account, |player, token| async move {
            player.reset_skin(&token).await
        })
        .await
    }

    /// Shows one of the account's capes by ID, or hides the cape with `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] with [`PlayerError::CapeNotOwned`] for a
    /// cape the account does not own.
    pub async fn set_cape(
        &self,
        account: &AccountId,
        cape: Option<&str>,
    ) -> Result<PlayerProfile, Error> {
        self.with_token(account, |player, token| {
            let cape = cape.map(str::to_owned);
            async move { player.set_cape(&token, cape.as_deref()).await }
        })
        .await
    }

    /// Runs a call with the account's token, renews the token once when it
    /// is rejected and saves the returned profile on the account.
    async fn with_token<F, Fut>(&self, account: &AccountId, call: F) -> Result<PlayerProfile, Error>
    where
        F: Fn(Player, Credential) -> Fut,
        Fut: Future<Output = Result<PlayerProfile, PlayerError>>,
    {
        let player = Player::new(std::sync::Arc::clone(&self.inner.transport));
        let mut mode = RefreshMode::IfNeeded;
        let profile = loop {
            let session = self
                .inner
                .accounts
                .session(account, mode)
                .await
                .map_err(auth_error)?;
            let token = session
                .access_token()
                .cloned()
                .ok_or(Error::Player(PlayerError::Offline))?;
            match call(player.clone(), token).await {
                Err(PlayerError::Unauthorized) if mode == RefreshMode::IfNeeded => {
                    mode = RefreshMode::Force;
                }
                result => break result?,
            }
        };
        self.inner
            .accounts
            .set_profile(account, profile.clone())
            .await?;
        Ok(profile)
    }
}
