use std::sync::Arc;

use coblet::auth::{PlayerProfile, SkinModel as EngineModel};
use coblet::player::{AppearanceCaps, AppearanceCtx, PlayerError, SkinSource, check_skin_png};
use coblet::runtime::{BoxFuture, Extension, Transport};
use coblet::types::ExtensionId;

use crate::http::{HttpRequest, HttpResponse, send};
use crate::java::extension_id;
use crate::{AccountInfo, CobletError, ErrorKind, PlayerInfo, SkinModel};

/// Service that shows and changes skins and capes, written in Swift,
/// Kotlin or Python, such as a skin site with its own API.
///
/// Calls run on a worker thread and may block. Register it with
/// `CobletBuilder.add_appearance_service`; auth methods name it, or
/// `CobletBuilder.set_appearance_for` binds one to it.
#[uniffi::export(foreign)]
pub trait AppearanceService: Send + Sync {
    /// Returns the ID, such as `acme:skins`; read once at registration.
    fn id(&self) -> String;

    /// Returns the name shown to users; read once at registration.
    fn display_name(&self) -> String;

    /// Returns what the service can change; coblet asks for nothing else.
    fn capabilities(&self) -> AppearanceCapabilities;

    /// Returns the profile with its skins and capes and which are active.
    ///
    /// # Errors
    ///
    /// Use `ErrorKind.Auth` when the token was rejected; coblet renews it
    /// and asks once more.
    fn appearance(&self, ctx: Arc<AppearanceContext>) -> Result<PlayerInfo, CobletError>;

    /// Makes a change the capabilities allow and returns the profile as
    /// it is afterwards.
    ///
    /// # Errors
    ///
    /// Use `ErrorKind.Auth` when the token was rejected, as for
    /// `appearance`.
    fn change(
        &self,
        ctx: Arc<AppearanceContext>,
        change: AppearanceChange,
    ) -> Result<PlayerInfo, CobletError>;
}

/// What an appearance service can change, so a launcher shows only the
/// controls that work.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent switches read by user interfaces"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct AppearanceCapabilities {
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

impl From<AppearanceCaps> for AppearanceCapabilities {
    fn from(caps: AppearanceCaps) -> Self {
        Self {
            upload_png: caps.upload_png,
            upload_url: caps.upload_url,
            set_model: caps.set_model,
            reset_skin: caps.reset_skin,
            choose_cape: caps.choose_cape,
            hide_cape: caps.hide_cape,
        }
    }
}

impl From<AppearanceCapabilities> for AppearanceCaps {
    fn from(caps: AppearanceCapabilities) -> Self {
        Self::none()
            .with_upload_png(caps.upload_png)
            .with_upload_url(caps.upload_url)
            .with_set_model(caps.set_model)
            .with_reset_skin(caps.reset_skin)
            .with_choose_cape(caps.choose_cape)
            .with_hide_cape(caps.hide_cape)
    }
}

/// Change asked of an [`AppearanceService`].
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppearanceChange {
    /// Upload a skin from PNG bytes, already checked, and make it active.
    UploadSkinPng {
        /// The image.
        png: Vec<u8>,
        /// Arm model.
        model: SkinModel,
    },
    /// Upload a skin from a public URL and make it active.
    UploadSkinUrl {
        /// The address.
        url: String,
        /// Arm model.
        model: SkinModel,
    },
    /// Switch the arm model of the active skin.
    SetModel {
        /// The new model.
        model: SkinModel,
    },
    /// Go back to the default skin.
    ResetSkin,
    /// Show one of the account's capes by ID, or hide the cape with `None`.
    SetCape {
        /// Cape ID.
        cape: Option<String>,
    },
}

impl AppearanceChange {
    const fn operation(&self) -> &'static str {
        match self {
            Self::UploadSkinPng { .. } => "upload skins from a file",
            Self::UploadSkinUrl { .. } => "upload skins from a URL",
            Self::SetModel { .. } => "change the skin model",
            Self::ResetSkin => "reset skins",
            Self::SetCape { cape: Some(_) } => "choose a cape",
            Self::SetCape { cape: None } => "hide the cape",
        }
    }
}

/// Appearance service known to a [`Coblet`](crate::Coblet).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppearanceServiceInfo {
    /// ID, such as `coblet:minecraft-services`.
    pub id: String,
    /// Name shown to users.
    pub name: String,
}

/// The account an appearance call is for, valid until the call returns.
#[derive(uniffi::Object)]
pub struct AppearanceContext {
    runtime: tokio::runtime::Handle,
    transport: Arc<dyn Transport>,
    account: AccountInfo,
    token: Option<String>,
}

impl std::fmt::Debug for AppearanceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppearanceContext").finish_non_exhaustive()
    }
}

#[uniffi::export]
impl AppearanceContext {
    /// Returns the account, with the profile saved last.
    #[must_use]
    pub fn account(&self) -> AccountInfo {
        self.account.clone()
    }

    /// Returns the access token of the account's current session.
    ///
    /// # Errors
    ///
    /// Fails with `Unsupported` for offline sessions.
    pub fn access_token(&self) -> Result<String, CobletError> {
        self.token
            .clone()
            .ok_or_else(|| CobletError::from(coblet::Error::Player(PlayerError::Offline)))
    }

    /// Sends an HTTP request through coblet's transport; any status comes
    /// back as a response.
    ///
    /// # Errors
    ///
    /// Fails for a malformed request and when the server cannot be reached.
    pub fn send(&self, request: HttpRequest) -> Result<HttpResponse, CobletError> {
        send(&self.runtime, &self.transport, request)
    }
}

/// Runs a foreign appearance service as a Rust one.
pub(crate) struct ForeignAppearance {
    id: ExtensionId,
    display_name: String,
    caps: AppearanceCaps,
    inner: Arc<dyn AppearanceService>,
}

impl ForeignAppearance {
    pub(crate) fn new(inner: Arc<dyn AppearanceService>) -> Result<Self, CobletError> {
        Ok(Self {
            id: extension_id("appearance service", &inner.id())?,
            display_name: inner.display_name(),
            caps: inner.capabilities().into(),
            inner,
        })
    }

    fn call<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        change: Option<AppearanceChange>,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        Box::pin(async move {
            let operation = change
                .as_ref()
                .map_or("read the appearance", AppearanceChange::operation);
            let context = Arc::new(AppearanceContext {
                runtime: tokio::runtime::Handle::current(),
                transport: Arc::clone(cx.transport()),
                account: AccountInfo::from(cx.account()),
                token: cx
                    .access_token()
                    .ok()
                    .map(|token| token.expose().to_owned()),
            });
            let inner = Arc::clone(&self.inner);
            let player = tokio::task::spawn_blocking(move || match change {
                Some(change) => inner.change(context, change),
                None => inner.appearance(context),
            })
            .await
            .map_err(|_| PlayerError::Failed("the appearance service stopped".to_owned()))?
            .map_err(|error| self.failed(&error, operation))?;
            PlayerProfile::try_from(player).map_err(|error| PlayerError::InvalidResponse {
                endpoint: "appearance service",
                message: error.to_string(),
            })
        })
    }

    fn failed(&self, error: &CobletError, operation: &'static str) -> PlayerError {
        match error.kind() {
            ErrorKind::Auth => PlayerError::Unauthorized,
            ErrorKind::Unsupported => PlayerError::Unsupported {
                service: self.id.clone(),
                operation,
            },
            ErrorKind::InvalidArgument => PlayerError::InvalidSkin(error.to_string()),
            ErrorKind::InvalidState => PlayerError::NoSkin,
            _ => PlayerError::Failed(error.to_string()),
        }
    }
}

impl Extension for ForeignAppearance {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }
}

impl coblet::player::AppearanceService for ForeignAppearance {
    fn capabilities(&self) -> AppearanceCaps {
        self.caps
    }

    fn appearance<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        self.call(cx, None)
    }

    fn upload_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        source: SkinSource,
        model: EngineModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let model = model.into();
        let change = match source {
            SkinSource::Png(png) => {
                if let Err(error) = check_skin_png(&png) {
                    return Box::pin(async move { Err(error) });
                }
                AppearanceChange::UploadSkinPng { png, model }
            }
            SkinSource::Url(url) => AppearanceChange::UploadSkinUrl {
                url: url.to_string(),
                model,
            },
            _ => {
                return Box::pin(async move {
                    Err(PlayerError::Failed("unknown skin source".to_owned()))
                });
            }
        };
        self.call(cx, Some(change))
    }

    fn set_model<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        model: EngineModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        self.call(
            cx,
            Some(AppearanceChange::SetModel {
                model: model.into(),
            }),
        )
    }

    fn reset_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        self.call(cx, Some(AppearanceChange::ResetSkin))
    }

    fn set_cape<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        cape: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        let cape = cape.map(str::to_owned);
        self.call(cx, Some(AppearanceChange::SetCape { cape }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{AppearanceCapabilities, AppearanceChange, AppearanceContext, AppearanceService};
    use crate::auth_methods::{
        AccountChange, AuthMethod, LoginContext, LoginRequest, RefreshContext, RefreshMode,
        RefreshResult, SessionInfo,
    };
    use crate::{
        AccountInfo, AccountState, Coblet, CobletBuilder, CobletError, ErrorKind, LoginInteraction,
        PlayerInfo, SkinInfo, SkinModel,
    };

    const UUID: &str = "0f5c5f6b8a7d4e2fb1d3a8c9e0f1a2b3";

    fn player(skins: Vec<SkinInfo>) -> PlayerInfo {
        PlayerInfo {
            name: "Alex".to_owned(),
            uuid: UUID.to_owned(),
            skins,
            capes: Vec::new(),
        }
    }

    /// Login whose sessions carry the number of refreshes as the token.
    #[derive(Default)]
    struct Site {
        refreshes: AtomicUsize,
    }

    impl AuthMethod for Site {
        fn id(&self) -> String {
            "acme:login".to_owned()
        }

        fn display_name(&self) -> String {
            "Acme".to_owned()
        }

        fn appearance_service(&self) -> Option<String> {
            Some("acme:skins".to_owned())
        }

        fn login(
            &self,
            _ctx: Arc<LoginContext>,
            _request: LoginRequest,
        ) -> Result<AccountChange, CobletError> {
            Ok(AccountChange {
                player: Some(player(Vec::new())),
                state: AccountState::Ready,
                session_expires_at: None,
                data: HashMap::new(),
                secrets: HashMap::new(),
            })
        }

        fn refresh(
            &self,
            _ctx: Arc<RefreshContext>,
            account: AccountInfo,
            mode: RefreshMode,
        ) -> Result<RefreshResult, CobletError> {
            let mut count = self.refreshes.load(Ordering::SeqCst);
            if mode == RefreshMode::Force {
                count = self.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
            }
            Ok(RefreshResult {
                change: AccountChange {
                    player: account.player,
                    state: AccountState::Ready,
                    session_expires_at: None,
                    data: account.data,
                    secrets: HashMap::new(),
                },
                session: Some(SessionInfo {
                    player_name: "Alex".to_owned(),
                    uuid: UUID.to_owned(),
                    access_token: Some(format!("token-{count}")),
                    xuid: None,
                    client_id: None,
                    user_type: Some("mojang".to_owned()),
                    jvm_args: Vec::new(),
                    expires_at: None,
                }),
            })
        }
    }

    /// Skin site kept in memory that accepts only `token-1`.
    #[derive(Default)]
    struct Skins {
        skins: Mutex<Vec<SkinInfo>>,
        calls: Mutex<Vec<String>>,
    }

    impl Skins {
        fn answer(&self, ctx: &AppearanceContext) -> Result<PlayerInfo, CobletError> {
            if ctx.access_token()? != "token-1" {
                return Err(CobletError::new(ErrorKind::Auth, "token rejected"));
            }
            Ok(player(self.skins.lock().unwrap().clone()))
        }
    }

    impl AppearanceService for Skins {
        fn id(&self) -> String {
            "acme:skins".to_owned()
        }

        fn display_name(&self) -> String {
            "Acme Skins".to_owned()
        }

        fn capabilities(&self) -> AppearanceCapabilities {
            AppearanceCapabilities {
                upload_png: true,
                reset_skin: true,
                ..AppearanceCapabilities::default()
            }
        }

        fn appearance(&self, ctx: Arc<AppearanceContext>) -> Result<PlayerInfo, CobletError> {
            self.calls.lock().unwrap().push("appearance".to_owned());
            self.answer(&ctx)
        }

        fn change(
            &self,
            ctx: Arc<AppearanceContext>,
            change: AppearanceChange,
        ) -> Result<PlayerInfo, CobletError> {
            let name = ctx.account().player.map(|player| player.name);
            self.calls
                .lock()
                .unwrap()
                .push(format!("{name:?}: {}", change.operation()));
            self.answer(&ctx)?;
            let mut skins = self.skins.lock().unwrap();
            match change {
                AppearanceChange::UploadSkinPng { png, model } => {
                    *skins = vec![SkinInfo {
                        id: format!("skin-{}", png.len()),
                        url: "https://acme.test/skin.png".to_owned(),
                        model,
                        active: true,
                    }];
                }
                AppearanceChange::ResetSkin => skins.clear(),
                _ => return Err(CobletError::new(ErrorKind::Unsupported, "no")),
            }
            Ok(player(skins.clone()))
        }
    }

    fn skin_png() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&64_u32.to_be_bytes());
        png.extend_from_slice(&64_u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        png
    }

    async fn signed_in(dir: &tempfile::TempDir, skins: &Arc<Skins>) -> (Arc<Coblet>, AccountInfo) {
        let builder = CobletBuilder::new(
            dir.path().to_string_lossy().into_owned(),
            "coblet-uniffi-test/1.0".to_owned(),
        )
        .unwrap();
        builder.add_auth_method(Arc::new(Site::default())).unwrap();
        builder.add_appearance_service(skins.clone()).unwrap();
        let coblet = builder.build().unwrap();
        let flow = coblet
            .begin_login(
                "acme:login".to_owned(),
                LoginInteraction::DeviceCode,
                None,
                HashMap::new(),
                HashMap::new(),
            )
            .await
            .unwrap();
        let account = flow.finish().await.unwrap();
        (coblet, account)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn skins_go_through_the_service_the_method_names() {
        let dir = tempfile::tempdir().unwrap();
        let skins = Arc::new(Skins::default());
        let (coblet, account) = signed_in(&dir, &skins).await;
        assert!(
            coblet
                .appearance_services()
                .iter()
                .any(|service| { service.id == "acme:skins" && service.name == "Acme Skins" })
        );
        let caps = coblet
            .appearance_capabilities(account.id.clone())
            .await
            .unwrap();
        assert!(caps.upload_png && caps.reset_skin && !caps.choose_cape);
        // The first token is rejected, so the upload renews it and retries.
        let worn = coblet
            .upload_skin(account.id.clone(), skin_png(), SkinModel::Slim)
            .await
            .unwrap();
        assert_eq!(worn.skins[0].model, SkinModel::Slim);
        let saved = coblet.account(account.id.clone()).await.unwrap();
        assert_eq!(saved.player.unwrap().skins, worn.skins);
        let reset = coblet.reset_skin(account.id.clone()).await.unwrap();
        assert_eq!(reset.skins.len(), 0);
        assert_eq!(
            skins.calls.lock().unwrap().as_slice(),
            [
                "Some(\"Alex\"): upload skins from a file",
                "Some(\"Alex\"): upload skins from a file",
                "Some(\"Alex\"): reset skins",
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsupported_and_invalid_changes_never_reach_the_service() {
        let dir = tempfile::tempdir().unwrap();
        let skins = Arc::new(Skins::default());
        let (coblet, account) = signed_in(&dir, &skins).await;
        let cape = coblet
            .set_cape(account.id.clone(), Some("migrator".to_owned()))
            .await
            .unwrap_err();
        assert_eq!(cape.kind(), ErrorKind::Unsupported);
        let png = coblet
            .upload_skin(
                account.id.clone(),
                b"not a png".to_vec(),
                SkinModel::Classic,
            )
            .await
            .unwrap_err();
        assert_eq!(png.kind(), ErrorKind::InvalidArgument);
        assert_eq!(skins.calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn data_services_and_bindings_are_checked_at_build() {
        let builder = || {
            CobletBuilder::new("/tmp/coblet-uniffi-appearance".to_owned(), "t/1".to_owned())
                .unwrap()
        };
        let toml = builder();
        toml.add_appearance_service_toml(
            "id = \"acme:ygg\"\nkind = \"yggdrasil\"\napi_root = \"https://acme.test/api/yggdrasil\"\n"
                .to_owned(),
        )
        .unwrap();
        toml.set_appearance_for("coblet:offline", "acme:ygg")
            .unwrap();
        let coblet = toml.build().unwrap();
        assert!(
            coblet
                .appearance_services()
                .iter()
                .any(|service| service.id == "acme:ygg")
        );
        let unknown = builder();
        unknown
            .set_appearance_for("coblet:offline", "acme:none")
            .unwrap();
        assert_eq!(unknown.build().unwrap_err().kind(), ErrorKind::Unsupported);
        let removed = builder();
        removed
            .remove_appearance_service("coblet:minecraft-services")
            .unwrap();
        assert_eq!(removed.build().unwrap().appearance_services().len(), 0);
        let broken = builder();
        broken
            .add_appearance_service_json("{\"id\": \"acme:x\"}".to_owned())
            .unwrap();
        assert_eq!(
            broken.build().unwrap_err().kind(),
            ErrorKind::InvalidArgument
        );
    }
}
