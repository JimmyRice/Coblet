use std::sync::Arc;

use coblet_auth::{Account, LaunchSession, PlayerProfile, SkinModel};
use coblet_launch_command::Credential;
use coblet_runtime::{BoxFuture, Extension, Transport};

use crate::{PlayerError, SkinSource};

/// Service that shows and changes the skins and capes of an account, such
/// as Minecraft services or a skin site that follows the authlib-injector
/// specification.
///
/// Each auth method names the service of its accounts with
/// [`AuthMethod::appearance_service`](coblet_auth::AuthMethod::appearance_service),
/// and a launcher can bind a method to another service. Only
/// [`appearance`](Self::appearance) is required; the other calls fail with
/// [`PlayerError::Unsupported`] unless the service implements them and says
/// so in [`capabilities`](Self::capabilities). Every call that succeeds
/// returns the profile as it is afterwards.
pub trait AppearanceService: Extension {
    /// Returns what the service can change.
    fn capabilities(&self) -> AppearanceCaps;

    /// Returns the profile with its skins and capes and which are active.
    fn appearance<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>>;

    /// Uploads a skin and makes it active.
    fn upload_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        source: SkinSource,
        model: SkinModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let _ = (cx, source, model);
        unsupported(self, "upload skins")
    }

    /// Switches the arm model of the active skin.
    fn set_model<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        model: SkinModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let _ = (cx, model);
        unsupported(self, "change the skin model")
    }

    /// Goes back to the default skin.
    fn reset_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let _ = cx;
        unsupported(self, "reset skins")
    }

    /// Shows one of the account's capes by ID, or hides the cape with `None`.
    fn set_cape<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        cape: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let _ = (cx, cape);
        unsupported(self, "change capes")
    }
}

/// Returns a future that fails with [`PlayerError::Unsupported`].
fn unsupported<'a, S: AppearanceService + ?Sized>(
    service: &S,
    operation: &'static str,
) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
    let error = PlayerError::Unsupported {
        service: service.id().clone(),
        operation,
    };
    Box::pin(async move { Err(error) })
}

/// What an [`AppearanceService`] can change, so a launcher shows only the
/// controls that work.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent switches read by user interfaces"
)]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppearanceCaps {
    /// Uploads skins from PNG bytes.
    pub upload_png: bool,
    /// Uploads skins from a public URL.
    pub upload_url: bool,
    /// Switches between the Classic and Slim models.
    pub set_model: bool,
    /// Goes back to the default skin.
    pub reset_skin: bool,
    /// Shows one of the capes the account owns.
    pub choose_cape: bool,
    /// Hides the cape.
    pub hide_cape: bool,
}

impl AppearanceCaps {
    /// Returns capabilities with nothing changeable.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Returns capabilities with everything changeable.
    #[must_use]
    pub fn all() -> Self {
        Self {
            upload_png: true,
            upload_url: true,
            set_model: true,
            reset_skin: true,
            choose_cape: true,
            hide_cape: true,
        }
    }

    /// Returns whether a skin from `source` can be uploaded.
    #[must_use]
    pub fn uploads(&self, source: &SkinSource) -> bool {
        match source {
            SkinSource::Png(_) => self.upload_png,
            SkinSource::Url(_) => self.upload_url,
        }
    }
}

/// What an [`AppearanceService`] can see of the account it works for.
#[derive(Clone)]
pub struct AppearanceCtx {
    transport: Arc<dyn Transport>,
    account: Account,
    session: LaunchSession,
}

impl AppearanceCtx {
    /// Describes a call for `account`, signed in as `session`.
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>, account: Account, session: LaunchSession) -> Self {
        Self {
            transport,
            account,
            session,
        }
    }

    /// Returns the transport to send requests with.
    #[must_use]
    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    /// Returns the account, including the data its auth method kept, such
    /// as the address of its login server.
    #[must_use]
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Returns the session of the account.
    #[must_use]
    pub fn session(&self) -> &LaunchSession {
        &self.session
    }

    /// Returns the access token of the session.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Offline`] for an offline session.
    pub fn access_token(&self) -> Result<&Credential, PlayerError> {
        self.session.access_token().ok_or(PlayerError::Offline)
    }
}

impl std::fmt::Debug for AppearanceCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppearanceCtx")
            .field("account", &self.account.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use coblet_auth::{Account, AccountId, LaunchSession, PlayerProfile, SkinModel};
    use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
    use coblet_runtime::{BoxFuture, Extension, HttpUrl, Method};
    use coblet_types::{ExtensionId, PlayerName};

    use super::{AppearanceCaps, AppearanceCtx, AppearanceService};
    use crate::mock::Mock;
    use crate::{MinecraftServices, PlayerError, SkinSource};

    /// Service that only reads, as a skin site without uploads would.
    struct ReadOnly(ExtensionId);

    impl Extension for ReadOnly {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Read only"
        }
    }

    impl AppearanceService for ReadOnly {
        fn capabilities(&self) -> AppearanceCaps {
            AppearanceCaps::none()
        }

        fn appearance<'a>(
            &'a self,
            cx: &'a AppearanceCtx,
        ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
            Box::pin(async move { Ok(cx.account().profile.clone().unwrap()) })
        }
    }

    fn notch() -> PlayerProfile {
        PlayerProfile::new(
            PlayerName::new("Notch").unwrap(),
            "069a79f444e94726a5befca90e38aaf5",
        )
        .unwrap()
    }

    fn ctx(transport: std::sync::Arc<Mock>, online: bool) -> AppearanceCtx {
        let mut account = Account::new(
            AccountId::new("a1").unwrap(),
            ExtensionId::new("acme:login").unwrap(),
        );
        account.profile = Some(notch());
        let auth = if online {
            LaunchAuth::online(OnlineSession::new(
                notch().name,
                notch().uuid,
                Credential::new("mc-token"),
            ))
        } else {
            LaunchAuth::offline(notch().name)
        };
        AppearanceCtx::new(transport, account, LaunchSession::new(auth))
    }

    #[tokio::test]
    async fn changes_a_service_cannot_make_fail_as_unsupported() {
        let service = ReadOnly(ExtensionId::new("acme:read-only").unwrap());
        let cx = ctx(Mock::new(), true);
        assert_eq!(service.appearance(&cx).await.unwrap(), notch());
        let error = service.reset_skin(&cx).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "appearance service `acme:read-only` cannot reset skins"
        );
        assert!(matches!(
            service.set_cape(&cx, None).await,
            Err(PlayerError::Unsupported {
                operation: "change capes",
                ..
            })
        ));
        let png = SkinSource::Png(Vec::new());
        assert!(!service.capabilities().uploads(&png));
        assert!(AppearanceCaps::all().uploads(&png));
    }

    #[tokio::test]
    async fn minecraft_services_take_the_token_from_the_session() {
        let mock = Mock::new();
        mock.on(
            Method::Get,
            "https://api.minecraftservices.com/minecraft/profile",
            200,
            r#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch"}"#,
        );
        let services = MinecraftServices::new(mock.clone());
        assert_eq!(services.id().as_str(), coblet_auth::MINECRAFT_SERVICES);
        let profile = AppearanceService::appearance(&services, &ctx(mock.clone(), true))
            .await
            .unwrap();
        assert_eq!(profile, notch());
        assert_eq!(
            mock.seen()[0].header("authorization"),
            Some("Bearer mc-token")
        );
        let offline = ctx(mock.clone(), false);
        let skin = SkinSource::Url(HttpUrl::new("https://example.com/s.png").unwrap());
        assert!(matches!(
            AppearanceService::upload_skin(&services, &offline, skin, SkinModel::Slim).await,
            Err(PlayerError::Offline)
        ));
        assert_eq!(mock.seen().len(), 1);
    }
}
