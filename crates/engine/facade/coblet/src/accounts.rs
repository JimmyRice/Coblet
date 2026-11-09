use coblet_auth::{
    Account, AccountId, AccountStore, AuthError, AuthFlow, LaunchSession, LoginOptions, RefreshMode,
};
use coblet_types::ExtensionId;

use crate::{Coblet, Error};

/// ID of the Microsoft auth method.
const MICROSOFT: &str = "coblet:microsoft";

impl Coblet {
    /// Returns the account store: the accounts in `data_root/accounts.json`,
    /// the default account and the account chosen for each instance.
    ///
    /// Reading it needs no network. The player profile of each account is
    /// the snapshot from its last login, token renewal or skin change; call
    /// [`refresh_profile`](Self::refresh_profile) for the current one.
    #[must_use]
    pub fn accounts(&self) -> &AccountStore {
        &self.inner.accounts
    }

    /// Returns the auth methods, such as `coblet:offline` and the ones
    /// added with [`auth_method`](crate::CobletBuilder::auth_method).
    #[must_use]
    pub fn auth_methods(&self) -> &coblet_runtime::Registry<dyn coblet_auth::AuthMethod> {
        self.inner.accounts.methods()
    }

    /// Starts a login with `method`, such as `coblet:offline` or
    /// `coblet:microsoft`. Show the flow's prompt, then pass the flow to
    /// [`finish_login`](Self::finish_login).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for Microsoft login without a Client
    /// ID, and [`Error::Auth`] when the method fails to start.
    pub async fn begin_login(
        &self,
        method: &ExtensionId,
        options: &LoginOptions,
    ) -> Result<AuthFlow, Error> {
        self.inner
            .accounts
            .begin_login(method, options)
            .await
            .map_err(auth_error)
    }

    /// Waits until the user has signed in and saves the account.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] with the reason the login failed.
    pub async fn finish_login(
        &self,
        method: &ExtensionId,
        flow: AuthFlow,
    ) -> Result<Account, Error> {
        self.inner
            .accounts
            .finish_login(method, flow)
            .await
            .map_err(auth_error)
    }

    /// Starts a Microsoft login.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn demo(coblet: coblet::Coblet) -> Result<(), coblet::Error> {
    /// use coblet::auth::LoginOptions;
    /// use coblet::runtime::AuthPrompt;
    ///
    /// let flow = coblet.begin_microsoft_login(&LoginOptions::new()).await?;
    /// if let AuthPrompt::DeviceCode { user_code, verification_uri, .. } = flow.prompt() {
    ///     println!("open {verification_uri} and enter {user_code}");
    /// }
    /// let account = coblet.finish_microsoft_login(flow).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] without a Client ID.
    #[cfg(feature = "auth-msa")]
    pub async fn begin_microsoft_login(&self, options: &LoginOptions) -> Result<AuthFlow, Error> {
        self.begin_login(&microsoft(), options).await
    }

    /// Waits for a Microsoft login and saves the account.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] with the reason the login failed.
    #[cfg(feature = "auth-msa")]
    pub async fn finish_microsoft_login(&self, flow: AuthFlow) -> Result<Account, Error> {
        self.finish_login(&microsoft(), flow).await
    }

    /// Returns a session to launch the account with, renewing its tokens
    /// first when they are about to expire.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] with [`AuthError::NoLicense`] when the
    /// account does not own the game (it can still launch in demo mode),
    /// [`Error::Unsupported`] for a Microsoft account without a Client ID,
    /// and the refresh error otherwise.
    pub async fn session(&self, account: &AccountId) -> Result<LaunchSession, Error> {
        self.inner
            .accounts
            .session(account, RefreshMode::IfNeeded)
            .await
            .map_err(auth_error)
    }

    /// Builds a launch request for the account, with its identity and any
    /// JVM arguments its auth method needs.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`session`](Self::session).
    #[cfg(feature = "launch")]
    pub async fn launch_request(
        &self,
        account: &AccountId,
    ) -> Result<coblet_launch::LaunchRequest, Error> {
        let session = self.session(account).await?;
        let mut request = coblet_launch::LaunchRequest::new(session.auth);
        request.jvm_args.extend(session.jvm_args);
        Ok(request)
    }
}

#[cfg(feature = "auth-msa")]
fn microsoft() -> ExtensionId {
    ExtensionId::new(MICROSOFT).unwrap_or_else(|_| unreachable!())
}

/// Reports a missing Microsoft method as unsupported rather than unknown.
pub(crate) fn auth_error(error: AuthError) -> Error {
    match error {
        AuthError::UnknownMethod(id) if id.as_str() == MICROSOFT => Error::Unsupported(
            if cfg!(feature = "auth-msa") {
                "microsoft login needs a client id and none was configured"
            } else {
                "microsoft login needs the auth-msa feature and a client id"
            }
            .to_owned(),
        ),
        AuthError::Unsupported(message) => Error::Unsupported(message),
        other => Error::Auth(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_auth::{AuthError, LoginOptions};
    use coblet_launch_command::LaunchAuth;
    use coblet_runtime::{BoxFuture, Request, Response, Transport, TransportError};
    use coblet_types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

    use crate::{Coblet, Error};

    struct NoNetwork;

    impl Transport for NoNetwork {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(async { Err(TransportError::Timeout) })
        }
    }

    fn builder(root: &std::path::Path) -> crate::CobletBuilder<crate::WithRoot> {
        Coblet::builder()
            .data_root(root)
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
    }

    #[tokio::test]
    async fn offline_accounts_launch_offline() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(dir.path())
            .transport(Arc::new(NoNetwork))
            .build()
            .unwrap();
        let steve = coblet
            .accounts()
            .add_offline(PlayerName::new("Steve").unwrap())
            .await
            .unwrap();
        assert!(dir.path().join("accounts.json").exists());
        let request = coblet.launch_request(&steve.id).await.unwrap();
        assert!(matches!(request.auth, LaunchAuth::Offline(ref name) if name.as_str() == "Steve"));
    }

    #[tokio::test]
    async fn microsoft_login_without_a_client_id_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(dir.path())
            .transport(Arc::new(NoNetwork))
            .build()
            .unwrap();
        let microsoft = ExtensionId::new("coblet:microsoft").unwrap();
        assert!(matches!(
            coblet.begin_login(&microsoft, &LoginOptions::new()).await,
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            coblet
                .begin_login(
                    &ExtensionId::new("acme:none").unwrap(),
                    &LoginOptions::new()
                )
                .await,
            Err(Error::Auth(AuthError::UnknownMethod(_)))
        ));
    }

    #[test]
    fn rejects_two_auth_methods_with_one_id() {
        let error = builder(std::path::Path::new("/tmp/x"))
            .transport(Arc::new(NoNetwork))
            .auth_method(Arc::new(coblet_auth::OfflineMethod::new()))
            .build()
            .unwrap_err();
        assert!(matches!(error, Error::Registry(_)), "{error}");
    }

    #[cfg(feature = "auth-msa")]
    mod msa {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use coblet_auth::{
            Account, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthFlow, AuthMethod,
            LaunchSession, MsaClientId, PlayerProfile, RefreshMode, Refreshed,
        };
        use coblet_launch_command::{Credential, OnlineSession};
        use coblet_player::PlayerError;
        use coblet_runtime::{AuthPrompt, Extension, MemoryBody};

        use super::*;

        const PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

        fn profile_json(alias: &str) -> String {
            format!(
                r#"{{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch", "skins": [],
                "capes": [{{"id": "c1", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/c", "alias": "{alias}"}}]}}"#
            )
        }

        /// Accepts only the token `fresh`.
        #[derive(Default)]
        struct Services {
            tokens: Mutex<Vec<String>>,
            urls: Mutex<Vec<String>>,
        }

        impl Transport for Services {
            fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
                let auth = request
                    .headers()
                    .iter()
                    .find(|(name, _)| name == "Authorization")
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default();
                self.tokens.lock().unwrap().push(auth.clone());
                self.urls
                    .lock()
                    .unwrap()
                    .push(request.url().as_str().to_owned());
                let ok = request.url().as_str() == PROFILE_URL && auth == "Bearer fresh";
                Box::pin(async move {
                    Ok(if ok {
                        Response::new(200, Vec::new(), MemoryBody::new(profile_json("Migrator")))
                    } else {
                        Response::new(401, Vec::new(), MemoryBody::default())
                    })
                })
            }
        }

        /// Hands out `stale` until forced to refresh, then `fresh`.
        struct Rotating {
            id: ExtensionId,
            forced: AtomicUsize,
        }

        impl Extension for Rotating {
            fn id(&self) -> &ExtensionId {
                &self.id
            }

            fn display_name(&self) -> &'static str {
                "Rotating"
            }
        }

        fn notch() -> PlayerProfile {
            PlayerProfile::new(
                PlayerName::new("Notch").unwrap(),
                "069a79f444e94726a5befca90e38aaf5",
            )
            .unwrap()
        }

        impl AuthMethod for Rotating {
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
                mode: RefreshMode,
            ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
                Box::pin(async move {
                    let token = if mode == RefreshMode::Force {
                        self.forced.fetch_add(1, Ordering::SeqCst);
                        "fresh"
                    } else {
                        "stale"
                    };
                    let session = LaunchSession::new(LaunchAuth::online(OnlineSession::new(
                        notch().name,
                        notch().uuid,
                        Credential::new(token),
                    )));
                    Ok(Refreshed::new(
                        AccountUpdate::from_account(account),
                        Some(session),
                    ))
                })
            }
        }

        #[tokio::test]
        async fn skin_calls_renew_a_rejected_token_once_and_save_the_profile() {
            let dir = tempfile::tempdir().unwrap();
            let services = Arc::new(Services::default());
            let method = Arc::new(Rotating {
                id: ExtensionId::new("test:rotating").unwrap(),
                forced: AtomicUsize::new(0),
            });
            let id = ExtensionId::new("test:rotating").unwrap();
            let coblet = builder(dir.path())
                .transport(services.clone())
                .auth_method(method.clone())
                .appearance_for(
                    id.clone(),
                    ExtensionId::new(coblet_auth::MINECRAFT_SERVICES).unwrap(),
                )
                .build()
                .unwrap();
            let flow = coblet.begin_login(&id, &LoginOptions::new()).await.unwrap();
            let account = coblet.finish_login(&id, flow).await.unwrap();
            assert_eq!(account.profile.as_ref().unwrap().capes.len(), 0);

            let refreshed = coblet.refresh_profile(&account.id).await.unwrap();
            let profile = refreshed.profile.unwrap();
            assert_eq!(
                profile.active_cape().unwrap().alias.as_deref(),
                Some("Migrator")
            );
            assert_eq!(method.forced.load(Ordering::SeqCst), 1);
            assert_eq!(
                *services.tokens.lock().unwrap(),
                ["Bearer stale", "Bearer fresh"]
            );
            let saved = coblet.accounts().get(&account.id).await.unwrap();
            assert_eq!(saved.profile.unwrap().capes.len(), 1);
        }

        #[tokio::test]
        async fn offline_accounts_cannot_change_skins_and_msa_needs_its_id() {
            let dir = tempfile::tempdir().unwrap();
            let coblet = builder(dir.path())
                .transport(Arc::new(NoNetwork))
                .msa_client_id(MsaClientId::new("00000000-0000-0000-0000-000000000000").unwrap())
                .build()
                .unwrap();
            assert!(
                coblet
                    .accounts()
                    .method(&ExtensionId::new("coblet:microsoft").unwrap())
                    .is_some()
            );
            let steve = coblet
                .accounts()
                .add_offline(PlayerName::new("Steve").unwrap())
                .await
                .unwrap();
            assert!(matches!(
                coblet.reset_skin(&steve.id).await,
                Err(Error::Player(PlayerError::Offline))
            ));
            let unchanged = coblet.refresh_profile(&steve.id).await.unwrap();
            assert_eq!(unchanged, steve);
        }

        #[tokio::test]
        async fn textures_download_over_https() {
            let dir = tempfile::tempdir().unwrap();
            let services = Arc::new(Services::default());
            let coblet = builder(dir.path())
                .transport(services.clone())
                .build()
                .unwrap();
            let url =
                coblet_runtime::HttpUrl::new("http://textures.minecraft.net/texture/abc").unwrap();
            let error = coblet.download_texture(&url).await.unwrap_err();
            assert!(matches!(error, Error::Player(_)), "{error}");
            assert_eq!(
                *services.urls.lock().unwrap(),
                ["https://textures.minecraft.net/texture/abc"]
            );
        }
    }
}
