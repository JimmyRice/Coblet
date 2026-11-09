use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use coblet_auth::{Account, AccountId, OFFLINE_METHOD, PlayerProfile, RefreshMode, SkinModel};
use coblet_player::{
    AppearanceCaps, AppearanceCtx, AppearanceService, MinecraftServices, PlayerError, SkinLibrary,
    SkinSource, YggdrasilAppearance,
};
use coblet_runtime::{HttpUrl, Registry, Transport};
use coblet_types::ExtensionId;

use crate::accounts::auth_error;
use crate::{Coblet, Error};

/// One appearance change made on the builder, applied in order.
pub(crate) enum AppearanceChange {
    Register(Arc<dyn AppearanceService>),
    Replace(Arc<dyn AppearanceService>),
    Remove(ExtensionId),
    Toml(String),
    Json(String),
    Bind(ExtensionId, ExtensionId),
}

/// Appearance changes collected by the builder.
#[derive(Default)]
pub(crate) struct AppearanceSettings {
    changes: Vec<AppearanceChange>,
}

/// Appearance services and the auth methods bound to them.
pub(crate) struct Appearance {
    services: Registry<dyn AppearanceService>,
    bindings: BTreeMap<ExtensionId, ExtensionId>,
}

impl AppearanceSettings {
    pub(crate) fn push(&mut self, change: AppearanceChange) {
        self.changes.push(change);
    }

    /// Starts from Minecraft services and applies the changes in order.
    pub(crate) fn build(self, transport: &Arc<dyn Transport>) -> Result<Appearance, Error> {
        let mut services: Registry<dyn AppearanceService> = Registry::new();
        services.register(Arc::new(MinecraftServices::new(Arc::clone(transport))))?;
        let mut bindings = BTreeMap::new();
        for change in self.changes {
            match change {
                AppearanceChange::Register(service) => services.register(service)?,
                AppearanceChange::Replace(service) => {
                    services.replace(service);
                }
                AppearanceChange::Remove(id) => {
                    services.remove(&id);
                }
                AppearanceChange::Toml(text) => services.register(Arc::new(
                    YggdrasilAppearance::from_toml(&text, Arc::clone(transport))?,
                ))?,
                AppearanceChange::Json(text) => services.register(Arc::new(
                    YggdrasilAppearance::from_json(&text, Arc::clone(transport))?,
                ))?,
                AppearanceChange::Bind(method, service) => {
                    bindings.insert(method, service);
                }
            }
        }
        for (method, service) in &bindings {
            if services.get(service).is_none() {
                return Err(Error::Unsupported(format!(
                    "appearance service `{service}` for `{method}` is not registered"
                )));
            }
        }
        Ok(Appearance { services, bindings })
    }
}

impl Coblet {
    /// Returns the local skin library in `data_root/skins/`.
    #[must_use]
    pub fn skins(&self) -> &SkinLibrary {
        &self.inner.skins
    }

    /// Returns the appearance services, such as `coblet:minecraft-services`.
    #[must_use]
    pub fn appearance_services(&self) -> &Registry<dyn AppearanceService> {
        &self.inner.appearance.services
    }

    /// Returns the service that shows and changes the account's skins and
    /// capes: the one bound to its auth method on the builder, else the
    /// one the method names, else `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] for an unknown account.
    pub async fn appearance_service(
        &self,
        account: &AccountId,
    ) -> Result<Option<Arc<dyn AppearanceService>>, Error> {
        let account = self.inner.accounts.get(account).await.map_err(auth_error)?;
        Ok(self.service_for(&account))
    }

    /// Returns what the account's appearance service can change, so a
    /// launcher shows only the controls that work; nothing for accounts
    /// without a service.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] for an unknown account.
    pub async fn appearance_capabilities(
        &self,
        account: &AccountId,
    ) -> Result<AppearanceCaps, Error> {
        Ok(self
            .appearance_service(account)
            .await?
            .map_or_else(AppearanceCaps::none, |service| service.capabilities()))
    }

    /// Fetches the account's player profile (name, UUID, skins and capes)
    /// from its appearance service now, saves it and returns the updated
    /// account.
    ///
    /// The profiles in [`accounts`](Self::accounts) are a snapshot saved at
    /// login, token renewal and skin or cape changes, so a change made on
    /// minecraft.net, a skin site or in another launcher shows up only
    /// after this call. A launcher can show the snapshot at once and call
    /// this in the background. It sends one request with the current token;
    /// use [`AccountStore::refresh`](coblet_auth::AccountStore::refresh) to
    /// renew the tokens themselves. Accounts without an appearance service,
    /// such as offline ones, are returned unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] when the account has to sign in again or has
    /// no profile, and [`Error::Player`] when the request fails.
    pub async fn refresh_profile(&self, account: &AccountId) -> Result<Account, Error> {
        let fetched = self
            .with_service(
                account,
                "read the appearance",
                |_| true,
                |service, cx| async move { service.appearance(&cx).await },
            )
            .await;
        match fetched {
            Ok(_) | Err(Error::Player(PlayerError::Offline | PlayerError::NoService(_))) => {
                self.inner.accounts.get(account).await.map_err(auth_error)
            }
            Err(error) => Err(error),
        }
    }

    /// Downloads a skin or cape texture, such as the `url` of a [`Skin`](coblet_auth::Skin),
    /// as PNG bytes, for example to draw the player's head.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] when the download fails or is larger than
    /// 256 KiB.
    pub async fn download_texture(&self, url: &HttpUrl) -> Result<Vec<u8>, Error> {
        let services = MinecraftServices::new(Arc::clone(&self.inner.transport));
        Ok(services.download(url).await?)
    }

    /// Uploads a skin for the account and makes it active.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] for an invalid image, an offline account,
    /// a service that cannot upload from this source or a failed request.
    pub async fn upload_skin(
        &self,
        account: &AccountId,
        source: SkinSource,
        model: SkinModel,
    ) -> Result<PlayerProfile, Error> {
        let operation = match source {
            SkinSource::Png(_) => "upload skins from a file",
            _ => "upload skins from a URL",
        };
        let allowed = |caps: &AppearanceCaps| caps.uploads(&source);
        self.with_service(account, operation, allowed, |service, cx| {
            let source = source.clone();
            async move { service.upload_skin(&cx, source, model).await }
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
        let allowed = |caps: &AppearanceCaps| caps.set_model;
        self.with_service(
            account,
            "change the skin model",
            allowed,
            |service, cx| async move { service.set_model(&cx, model).await },
        )
        .await
    }

    /// Goes back to the default skin.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Player`] when the request fails.
    pub async fn reset_skin(&self, account: &AccountId) -> Result<PlayerProfile, Error> {
        let allowed = |caps: &AppearanceCaps| caps.reset_skin;
        self.with_service(account, "reset skins", allowed, |service, cx| async move {
            service.reset_skin(&cx).await
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
        let (operation, allowed): (_, fn(&AppearanceCaps) -> bool) = match cape {
            Some(_) => ("choose a cape", |caps| caps.choose_cape),
            None => ("hide the cape", |caps| caps.hide_cape),
        };
        self.with_service(account, operation, allowed, |service, cx| {
            let cape = cape.map(str::to_owned);
            async move { service.set_cape(&cx, cape.as_deref()).await }
        })
        .await
    }

    fn service_for(&self, account: &Account) -> Option<Arc<dyn AppearanceService>> {
        let appearance = &self.inner.appearance;
        let id = match appearance.bindings.get(&account.method) {
            Some(bound) => bound.clone(),
            None => self
                .inner
                .accounts
                .method(&account.method)?
                .appearance_service()?,
        };
        appearance.services.get(&id).cloned()
    }

    /// Runs a call on the account's appearance service with its session,
    /// renews the session once when the token is rejected and saves the
    /// returned profile on the account.
    async fn with_service<F, Fut>(
        &self,
        account: &AccountId,
        operation: &'static str,
        allowed: impl Fn(&AppearanceCaps) -> bool,
        call: F,
    ) -> Result<PlayerProfile, Error>
    where
        F: Fn(Arc<dyn AppearanceService>, AppearanceCtx) -> Fut,
        Fut: Future<Output = Result<PlayerProfile, PlayerError>>,
    {
        let saved = self.inner.accounts.get(account).await.map_err(auth_error)?;
        let Some(service) = self.service_for(&saved) else {
            return Err(Error::Player(if saved.method.as_str() == OFFLINE_METHOD {
                PlayerError::Offline
            } else {
                PlayerError::NoService(saved.method)
            }));
        };
        if !allowed(&service.capabilities()) {
            return Err(Error::Player(PlayerError::Unsupported {
                service: service.id().clone(),
                operation,
            }));
        }
        let mut mode = RefreshMode::IfNeeded;
        let profile = loop {
            let session = self
                .inner
                .accounts
                .session(account, mode)
                .await
                .map_err(auth_error)?;
            let saved = self.inner.accounts.get(account).await.map_err(auth_error)?;
            let cx = AppearanceCtx::new(Arc::clone(&self.inner.transport), saved, session);
            match call(Arc::clone(&service), cx).await {
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use coblet_auth::{
        Account, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError, AuthFlow,
        AuthMethod, LaunchSession, LoginOptions, PlayerProfile, RefreshMode, Refreshed, Skin,
        SkinModel,
    };
    use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
    use coblet_player::{
        AppearanceCaps, AppearanceCtx, AppearanceService, PlayerError, SkinSource,
    };
    use coblet_runtime::{
        AuthPrompt, BoxFuture, Extension, HttpUrl, Request, Response, Transport, TransportError,
    };
    use coblet_types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

    use crate::{Coblet, Error};

    struct NoNetwork;

    impl Transport for NoNetwork {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(async { Err(TransportError::Timeout) })
        }
    }

    fn id(text: &str) -> ExtensionId {
        ExtensionId::new(text).unwrap()
    }

    fn notch() -> PlayerProfile {
        PlayerProfile::new(
            PlayerName::new("Notch").unwrap(),
            "069a79f444e94726a5befca90e38aaf5",
        )
        .unwrap()
    }

    /// Login of a skin site; it names the site's appearance service.
    struct SiteLogin {
        id: ExtensionId,
        service: Option<ExtensionId>,
    }

    impl Extension for SiteLogin {
        fn id(&self) -> &ExtensionId {
            &self.id
        }

        fn display_name(&self) -> &'static str {
            "Site login"
        }
    }

    impl AuthMethod for SiteLogin {
        fn begin<'a>(
            &'a self,
            _ctx: &'a AuthCtx,
            _options: &'a LoginOptions,
        ) -> BoxFuture<'a, Result<AuthFlow, AuthError>> {
            Box::pin(async {
                let update = AccountUpdate::new(AccountState::Ready).with_profile(notch());
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
            _secrets: &'a AccountSecrets,
            _mode: RefreshMode,
        ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
            Box::pin(async move {
                let session = LaunchSession::new(LaunchAuth::online(OnlineSession::new(
                    notch().name,
                    notch().uuid,
                    Credential::new("site-token"),
                )));
                Ok(Refreshed::new(
                    AccountUpdate::from_account(account),
                    Some(session),
                ))
            })
        }

        fn appearance_service(&self) -> Option<ExtensionId> {
            self.service.clone()
        }
    }

    /// Skin site kept in memory that takes PNG uploads and nothing else.
    struct Memory {
        id: ExtensionId,
        tokens: Mutex<Vec<String>>,
    }

    impl Extension for Memory {
        fn id(&self) -> &ExtensionId {
            &self.id
        }

        fn display_name(&self) -> &'static str {
            "Memory"
        }
    }

    impl AppearanceService for Memory {
        fn capabilities(&self) -> AppearanceCaps {
            AppearanceCaps::none().with_upload_png(true)
        }

        fn appearance<'a>(
            &'a self,
            _cx: &'a AppearanceCtx,
        ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
            Box::pin(async { Ok(notch()) })
        }

        fn upload_skin<'a>(
            &'a self,
            cx: &'a AppearanceCtx,
            _source: SkinSource,
            model: SkinModel,
        ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
            Box::pin(async move {
                let token = cx.access_token()?.expose().to_owned();
                self.tokens.lock().unwrap().push(token);
                let mut profile = cx.account().profile.clone().unwrap();
                let url = HttpUrl::new("https://skins.example/textures/new").unwrap();
                profile.skins = vec![Skin::new("new", url, model, true)];
                Ok(profile)
            })
        }
    }

    fn builder(root: &std::path::Path) -> crate::CobletBuilder<crate::WithRoot> {
        Coblet::builder()
            .data_root(root)
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .transport(Arc::new(NoNetwork))
    }

    async fn sign_in(coblet: &Coblet, method: &str) -> Account {
        let method = id(method);
        let flow = coblet
            .begin_login(&method, &LoginOptions::new())
            .await
            .unwrap();
        coblet.finish_login(&method, flow).await.unwrap()
    }

    #[tokio::test]
    async fn accounts_use_the_service_their_method_names_within_its_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let memory = Arc::new(Memory {
            id: id("acme:skins"),
            tokens: Mutex::new(Vec::new()),
        });
        let coblet = builder(dir.path())
            .auth_method(Arc::new(SiteLogin {
                id: id("acme:login"),
                service: Some(id("acme:skins")),
            }))
            .appearance_service(memory.clone())
            .build()
            .unwrap();
        let account = sign_in(&coblet, "acme:login").await;
        let caps = coblet.appearance_capabilities(&account.id).await.unwrap();
        assert!(caps.upload_png && !caps.upload_url && !caps.reset_skin);

        let png = SkinSource::Png(Vec::new());
        let profile = coblet
            .upload_skin(&account.id, png, SkinModel::Slim)
            .await
            .unwrap();
        assert_eq!(profile.active_skin().unwrap().model, SkinModel::Slim);
        let saved = coblet.accounts().get(&account.id).await.unwrap();
        assert_eq!(saved.profile.unwrap().active_skin().unwrap().id, "new");
        assert_eq!(*memory.tokens.lock().unwrap(), ["site-token"]);

        let url = SkinSource::Url(HttpUrl::new("https://example.com/s.png").unwrap());
        let error = coblet
            .upload_skin(&account.id, url, SkinModel::Classic)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "appearance service `acme:skins` cannot upload skins from a URL"
        );
        assert!(matches!(
            coblet.set_cape(&account.id, None).await,
            Err(Error::Player(PlayerError::Unsupported {
                operation: "hide the cape",
                ..
            }))
        ));
        assert_eq!(memory.tokens.lock().unwrap().len(), 1);

        let steve = coblet
            .accounts()
            .add_offline(PlayerName::new("Steve").unwrap())
            .await
            .unwrap();
        assert!(matches!(
            coblet.reset_skin(&steve.id).await,
            Err(Error::Player(PlayerError::Offline))
        ));
        assert_eq!(
            coblet.appearance_capabilities(&steve.id).await.unwrap(),
            AppearanceCaps::none()
        );
    }

    #[tokio::test]
    async fn bindings_win_over_the_method_and_definitions_add_services() {
        let dir = tempfile::tempdir().unwrap();
        let definition = r#"
            id = "acme:site"
            kind = "yggdrasil"
            api_root = "https://skins.example/api/yggdrasil"
        "#;
        let coblet = builder(dir.path())
            .auth_method(Arc::new(SiteLogin {
                id: id("acme:login"),
                service: Some(id("coblet:minecraft-services")),
            }))
            .auth_method(Arc::new(SiteLogin {
                id: id("acme:bare"),
                service: None,
            }))
            .replace_auth_method(Arc::new(coblet_auth::OfflineMethod::new()))
            .appearance_service_toml(definition)
            .appearance_for(id("acme:login"), id("acme:site"))
            .build()
            .unwrap();
        let ids: Vec<_> = coblet
            .appearance_services()
            .iter()
            .map(|service| service.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["acme:site", "coblet:minecraft-services"]);
        let names: Vec<_> = coblet
            .auth_methods()
            .iter()
            .map(|method| method.id().as_str().to_owned())
            .collect();
        assert_eq!(names, ["acme:bare", "acme:login", "coblet:offline"]);

        let account = sign_in(&coblet, "acme:login").await;
        let service = coblet
            .appearance_service(&account.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(service.id().as_str(), "acme:site");
        let bare = sign_in(&coblet, "acme:bare").await;
        assert!(coblet.appearance_service(&bare.id).await.unwrap().is_none());
        assert_eq!(
            coblet.reset_skin(&bare.id).await.unwrap_err().to_string(),
            "accounts of `acme:bare` have no appearance service"
        );
        assert_eq!(coblet.refresh_profile(&bare.id).await.unwrap(), bare);
    }

    #[test]
    fn unknown_bindings_and_broken_definitions_fail_the_build() {
        let root = std::path::Path::new("/tmp/coblet-appearance");
        let error = builder(root)
            .appearance_for(id("acme:login"), id("acme:missing"))
            .build()
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "appearance service `acme:missing` for `acme:login` is not registered"
        );
        let error = builder(root)
            .appearance_service_json(
                r#"{"id": "acme:site", "kind": "nope", "api_root": "https://x.example"}"#,
            )
            .build()
            .unwrap_err();
        assert!(
            matches!(error, Error::Player(PlayerError::Definition(_))),
            "{error}"
        );
    }
}
