use std::sync::Arc;

use coblet_auth::{Cape, PlayerProfile, Skin, SkinModel};
use coblet_runtime::{BoxFuture, Extension, HttpUrl, Method, Request, Transport};
use coblet_types::ExtensionId;
use serde::Deserialize;

use crate::http::{Reply, multipart, send, url};
use crate::player::public_player;
use crate::{
    AppearanceCaps, AppearanceCtx, AppearanceService, MAX_SKIN_BYTES, PlayerError, SkinSource,
    check_skin_png,
};

/// Skins and capes on a skin site that follows the authlib-injector
/// specification, such as `LittleSkin` or a Blessing Skin server.
///
/// It reads the profile from the site's session server, uploads PNG skins,
/// switches the model by uploading the current skin again, resets the skin
/// and hides the cape. The specification has no list of owned capes and no
/// upload from a URL, so those changes are not offered. Uploads need the
/// access token the site issued, so the accounts have to come from a login
/// method for the same site.
///
/// # Examples
///
/// ```
/// use coblet_player::{AppearanceService, YggdrasilAppearance};
/// # use std::sync::Arc;
/// # use coblet_runtime::{BoxFuture, Request, Response, Transport, TransportError};
/// # struct NoNetwork;
/// # impl Transport for NoNetwork {
/// #     fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
/// #         Box::pin(async { Err(TransportError::Timeout) })
/// #     }
/// # }
///
/// let site = YggdrasilAppearance::from_toml(
///     r#"
///     id = "acme:skins"
///     name = "Acme skins"
///     kind = "yggdrasil"
///     api_root = "https://skins.example/api/yggdrasil"
///     "#,
///     Arc::new(NoNetwork),
/// )?;
/// assert!(site.capabilities().upload_png);
/// assert!(!site.capabilities().choose_cape);
/// # Ok::<(), coblet_player::PlayerError>(())
/// ```
#[derive(Clone)]
pub struct YggdrasilAppearance {
    id: ExtensionId,
    name: String,
    api_root: String,
    transport: Arc<dyn Transport>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    name: Option<String>,
    kind: String,
    api_root: String,
}

#[derive(Deserialize)]
struct RawProperties {
    #[serde(default)]
    properties: Vec<RawProperty>,
}

#[derive(Deserialize)]
struct RawProperty {
    name: String,
    value: String,
}

impl YggdrasilAppearance {
    /// Creates the service for the site whose API starts at `api_root`,
    /// such as `https://littleskin.cn/api/yggdrasil`.
    #[must_use]
    pub fn new(
        id: ExtensionId,
        name: impl Into<String>,
        api_root: &HttpUrl,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            api_root: api_root.as_str().trim_end_matches('/').to_owned(),
            transport,
        }
    }

    /// Parses a definition in TOML: `id`, `kind = "yggdrasil"`, `api_root`
    /// and an optional `name`.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Definition`] when the text is malformed.
    pub fn from_toml(text: &str, transport: Arc<dyn Transport>) -> Result<Self, PlayerError> {
        let definition: Definition =
            toml::from_str(text).map_err(|error| PlayerError::Definition(error.to_string()))?;
        Self::from_definition(definition, transport)
    }

    /// Parses a definition in JSON, with the same fields as the TOML form.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Definition`] when the text is malformed.
    pub fn from_json(text: &str, transport: Arc<dyn Transport>) -> Result<Self, PlayerError> {
        let definition: Definition = serde_json::from_str(text)
            .map_err(|error| PlayerError::Definition(error.to_string()))?;
        Self::from_definition(definition, transport)
    }

    fn from_definition(
        definition: Definition,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, PlayerError> {
        let invalid = PlayerError::Definition;
        let id = ExtensionId::new(&definition.id)
            .map_err(|error| invalid(format!("id `{}`: {error}", definition.id)))?;
        if definition.kind != "yggdrasil" {
            return Err(invalid(format!(
                "kind `{}` is not known; the only kind is `yggdrasil`",
                definition.kind
            )));
        }
        let api_root = HttpUrl::new(&definition.api_root)
            .map_err(|error| invalid(format!("api_root: {error}")))?;
        let name = definition.name.unwrap_or_else(|| definition.id.clone());
        Ok(Self::new(id, name, &api_root, transport))
    }

    /// Returns the address the site's API starts at.
    #[must_use]
    pub fn api_root(&self) -> &str {
        &self.api_root
    }

    /// Reads the profile and the textures the site lets the player upload.
    async fn read(&self, cx: &AppearanceCtx) -> Result<(PlayerProfile, Vec<String>), PlayerError> {
        let uuid = uuid(cx)?;
        let target = format!(
            "{}/sessionserver/session/minecraft/profile/{uuid}",
            self.api_root
        );
        let reply = send(
            &self.transport,
            "session profile",
            Request::get(url(&target)?),
            None,
        )
        .await?;
        if matches!(reply.status, 204 | 404) {
            return Err(PlayerError::UnknownPlayer(uuid));
        }
        if !reply.is_success() {
            return Err(reply.error());
        }
        let public = public_player(&reply)?;
        let mut profile = PlayerProfile::new(public.name, &public.uuid).map_err(|error| {
            PlayerError::InvalidResponse {
                endpoint: reply.endpoint,
                message: error.to_string(),
            }
        })?;
        if let Some(skin) = public.skin {
            profile
                .skins
                .push(Skin::new(texture_id(&skin), skin, public.model, true));
        }
        if let Some(cape) = public.cape {
            profile
                .capes
                .push(Cape::new(texture_id(&cape), None, cape, true));
        }
        Ok((profile, uploadable(&reply)?))
    }

    /// Sends an authorized texture call and reads the profile afterwards.
    async fn change(
        &self,
        cx: &AppearanceCtx,
        method: Method,
        texture: &'static str,
        body: Option<(String, Vec<u8>)>,
    ) -> Result<PlayerProfile, PlayerError> {
        let target = format!("{}/api/user/profile/{}/{texture}", self.api_root, uuid(cx)?);
        let token = cx.access_token()?;
        let mut request = Request::new(method, url(&target)?)
            .header("Authorization", format!("Bearer {}", token.expose()));
        if let Some((boundary, bytes)) = body {
            request = request
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(bytes);
        }
        let reply = send(&self.transport, texture, request, None).await?;
        if !reply.is_success() {
            return Err(reply.error());
        }
        Ok(self.read(cx).await?.0)
    }

    async fn upload(
        &self,
        cx: &AppearanceCtx,
        png: &[u8],
        model: SkinModel,
    ) -> Result<PlayerProfile, PlayerError> {
        check_skin_png(png)?;
        let (_, uploadable) = self.read(cx).await?;
        if !uploadable.iter().any(|texture| texture == "skin") {
            return Err(self.unsupported("upload skins for this player"));
        }
        let boundary = format!("coblet-{}", uuid::Uuid::new_v4().simple());
        let value = match model {
            SkinModel::Slim => "slim",
            SkinModel::Classic => "",
        };
        let body = multipart(&boundary, "model", value, png);
        self.change(cx, Method::Put, "skin", Some((boundary, body)))
            .await
    }

    fn unsupported(&self, operation: &'static str) -> PlayerError {
        PlayerError::Unsupported {
            service: self.id.clone(),
            operation,
        }
    }
}

/// Returns the profile UUID of the account.
fn uuid(cx: &AppearanceCtx) -> Result<String, PlayerError> {
    if let Some(profile) = &cx.account().profile {
        return Ok(profile.uuid.clone());
    }
    match &cx.session().auth {
        coblet_launch_command::LaunchAuth::Online(session) => Ok(session.uuid.clone()),
        _ => Err(PlayerError::Offline),
    }
}

/// Names a texture by the last part of its URL, which is its hash on
/// authlib-injector compatible sites.
fn texture_id(texture: &HttpUrl) -> String {
    texture
        .as_str()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Reads the `uploadableTextures` property, such as `skin,cape`; without
/// it nothing can be uploaded.
fn uploadable(reply: &Reply) -> Result<Vec<String>, PlayerError> {
    let raw: RawProperties = reply.json()?;
    Ok(raw
        .properties
        .into_iter()
        .find(|property| property.name == "uploadableTextures")
        .map(|property| {
            property
                .value
                .split(',')
                .map(|texture| texture.trim().to_owned())
                .collect()
        })
        .unwrap_or_default())
}

impl Extension for YggdrasilAppearance {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl AppearanceService for YggdrasilAppearance {
    fn capabilities(&self) -> AppearanceCaps {
        AppearanceCaps {
            upload_png: true,
            set_model: true,
            reset_skin: true,
            hide_cape: true,
            ..AppearanceCaps::none()
        }
    }

    fn appearance<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        Box::pin(async move { Ok(self.read(cx).await?.0) })
    }

    fn upload_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        source: SkinSource,
        model: SkinModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        Box::pin(async move {
            match source {
                SkinSource::Png(png) => self.upload(cx, &png, model).await,
                SkinSource::Url(_) => Err(self.unsupported("upload skins from a URL")),
            }
        })
    }

    fn set_model<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        model: SkinModel,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        Box::pin(async move {
            let (current, _) = self.read(cx).await?;
            let skin = current.active_skin().ok_or(PlayerError::NoSkin)?;
            if skin.model == model {
                return Ok(current);
            }
            let limit = u64::try_from(MAX_SKIN_BYTES).unwrap_or(u64::MAX);
            let reply = send(
                &self.transport,
                "texture",
                Request::get(skin.url.clone()),
                Some(limit),
            )
            .await?;
            if !reply.is_success() {
                return Err(reply.error());
            }
            self.upload(cx, &reply.body, model).await
        })
    }

    fn reset_skin<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        Box::pin(async move { self.change(cx, Method::Delete, "skin", None).await })
    }

    fn set_cape<'a>(
        &'a self,
        cx: &'a AppearanceCtx,
        cape: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PlayerProfile, PlayerError>> {
        Box::pin(async move {
            match cape {
                Some(_) => Err(self.unsupported("choose a cape")),
                None => self.change(cx, Method::Delete, "cape", None).await,
            }
        })
    }
}

impl std::fmt::Debug for YggdrasilAppearance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YggdrasilAppearance")
            .field("id", &self.id)
            .field("api_root", &self.api_root)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use coblet_auth::{Account, AccountId, LaunchSession, SkinModel};
    use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
    use coblet_runtime::{HttpUrl, Method};
    use coblet_types::{ExtensionId, PlayerName};

    use super::YggdrasilAppearance;
    use crate::mock::Mock;
    use crate::png::sample;
    use crate::{AppearanceCtx, AppearanceService, PlayerError, SkinSource};

    const ROOT: &str = "https://skins.example/api/yggdrasil";
    const UUID: &str = "069a79f444e94726a5befca90e38aaf5";
    const PROFILE: &str = "https://skins.example/api/yggdrasil/sessionserver/session/minecraft/profile/069a79f444e94726a5befca90e38aaf5";
    const SKIN: &str = "https://skins.example/api/yggdrasil/api/user/profile/069a79f444e94726a5befca90e38aaf5/skin";
    const CAPE: &str = "https://skins.example/api/yggdrasil/api/user/profile/069a79f444e94726a5befca90e38aaf5/cape";
    const TEXTURE: &str = "https://skins.example/textures/abc123";

    /// Answers like the session server of a skin site.
    fn profile_json(slim: bool, uploadable: Option<&str>) -> String {
        let mut skin = serde_json::json!({ "url": TEXTURE });
        if slim {
            skin["metadata"] = serde_json::json!({ "model": "slim" });
        }
        let textures = serde_json::json!({ "textures": {
            "SKIN": skin,
            "CAPE": { "url": "https://skins.example/textures/cape9" },
        }});
        let mut properties = vec![serde_json::json!({
            "name": "textures",
            "value": STANDARD.encode(textures.to_string()),
        })];
        if let Some(value) = uploadable {
            properties.push(serde_json::json!({ "name": "uploadableTextures", "value": value }));
        }
        serde_json::json!({ "id": UUID, "name": "Notch", "properties": properties }).to_string()
    }

    fn site(mock: Arc<Mock>) -> YggdrasilAppearance {
        YggdrasilAppearance::from_toml(
            &format!("id = \"acme:skins\"\nkind = \"yggdrasil\"\napi_root = \"{ROOT}/\"\n"),
            mock,
        )
        .unwrap()
    }

    fn ctx(mock: Arc<Mock>) -> AppearanceCtx {
        let name = PlayerName::new("Notch").unwrap();
        let session = OnlineSession::new(name, UUID, Credential::new("site-token"));
        let account = Account::new(
            AccountId::new("a1").unwrap(),
            ExtensionId::new("acme:login").unwrap(),
        );
        AppearanceCtx::new(
            mock,
            account,
            LaunchSession::new(LaunchAuth::online(session)),
        )
    }

    #[test]
    fn definitions_are_checked() {
        let mock = Mock::new();
        let json = format!(
            r#"{{"id": "acme:skins", "name": "Acme", "kind": "yggdrasil", "api_root": "{ROOT}"}}"#
        );
        let parsed = YggdrasilAppearance::from_json(&json, mock.clone()).unwrap();
        assert_eq!(coblet_runtime::Extension::display_name(&parsed), "Acme");
        assert_eq!(parsed.api_root(), ROOT);
        assert_eq!(
            coblet_runtime::Extension::display_name(&site(mock.clone())),
            "acme:skins"
        );
        for (text, wanted) in [
            (
                "id = \"acme:skins\"\nkind = \"mojang\"\napi_root = \"https://x.example\"",
                "kind `mojang`",
            ),
            (
                "id = \"No Namespace\"\nkind = \"yggdrasil\"\napi_root = \"https://x.example\"",
                "id `No Namespace`",
            ),
            (
                "id = \"acme:skins\"\nkind = \"yggdrasil\"\napi_root = \"not a url\"",
                "api_root",
            ),
            (
                "id = \"acme:skins\"\nkind = \"yggdrasil\"\napi_root = \"https://x.example\"\nextra = 1",
                "extra",
            ),
        ] {
            let error = YggdrasilAppearance::from_toml(text, mock.clone()).unwrap_err();
            assert!(matches!(error, PlayerError::Definition(_)), "{error}");
            assert!(error.to_string().contains(wanted), "{error}");
        }
    }

    #[tokio::test]
    async fn reads_skins_and_capes_from_the_session_server() {
        let mock = Mock::new();
        mock.on(Method::Get, PROFILE, 200, profile_json(true, None));
        let profile = site(mock.clone())
            .appearance(&ctx(mock.clone()))
            .await
            .unwrap();
        let skin = profile.active_skin().unwrap();
        assert_eq!((skin.id.as_str(), skin.model), ("abc123", SkinModel::Slim));
        assert_eq!(profile.active_cape().unwrap().id, "cape9");
        assert_eq!(profile.uuid, UUID);
    }

    #[tokio::test]
    async fn uploads_png_skins_with_the_site_token() {
        let mock = Mock::new();
        mock.on(
            Method::Get,
            PROFILE,
            200,
            profile_json(true, Some("skin,cape")),
        )
        .on(Method::Put, SKIN, 204, "");
        let png = sample(64, 64);
        let source = SkinSource::Png(png.clone());
        site(mock.clone())
            .upload_skin(&ctx(mock.clone()), source, SkinModel::Slim)
            .await
            .unwrap();
        let put = &mock.seen_with(Method::Put)[0];
        assert_eq!(put.header("authorization"), Some("Bearer site-token"));
        let text = put.text();
        assert!(text.contains("name=\"model\"\r\n\r\nslim\r\n"), "{text}");
        assert!(put.body.windows(png.len()).any(|window| window == png));
        assert_eq!(mock.seen_with(Method::Get).len(), 2);

        let url = SkinSource::Url(HttpUrl::new("https://example.com/s.png").unwrap());
        let error = site(mock.clone())
            .upload_skin(&ctx(mock.clone()), url, SkinModel::Classic)
            .await
            .unwrap_err();
        assert!(matches!(error, PlayerError::Unsupported { .. }), "{error}");
        assert_eq!(mock.seen_with(Method::Put).len(), 1);
    }

    #[tokio::test]
    async fn sites_that_allow_no_uploads_are_not_asked() {
        let mock = Mock::new();
        mock.on(Method::Get, PROFILE, 200, profile_json(false, None));
        let error = site(mock.clone())
            .upload_skin(
                &ctx(mock.clone()),
                SkinSource::Png(sample(64, 64)),
                SkinModel::Classic,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "appearance service `acme:skins` cannot upload skins for this player"
        );
        assert!(mock.seen_with(Method::Put).is_empty());
    }

    #[tokio::test]
    async fn switches_the_model_by_uploading_the_skin_again() {
        let mock = Mock::new();
        mock.on(Method::Get, PROFILE, 200, profile_json(false, Some("skin")))
            .on(Method::Get, TEXTURE, 200, sample(64, 64))
            .on(Method::Put, SKIN, 204, "");
        let site = site(mock.clone());
        site.set_model(&ctx(mock.clone()), SkinModel::Classic)
            .await
            .unwrap();
        assert!(mock.seen_with(Method::Put).is_empty());
        site.set_model(&ctx(mock.clone()), SkinModel::Slim)
            .await
            .unwrap();
        let put = &mock.seen_with(Method::Put)[0];
        assert!(put.text().contains("name=\"model\"\r\n\r\nslim\r\n"));
        assert!(mock.seen().iter().any(|seen| seen.url == TEXTURE));
    }

    #[tokio::test]
    async fn resets_skins_and_hides_capes() {
        let mock = Mock::new();
        mock.on(Method::Get, PROFILE, 200, profile_json(false, None))
            .on(Method::Delete, SKIN, 204, "")
            .on(Method::Delete, CAPE, 204, "");
        let site = site(mock.clone());
        let cx = ctx(mock.clone());
        site.reset_skin(&cx).await.unwrap();
        site.set_cape(&cx, None).await.unwrap();
        let deleted: Vec<_> = mock
            .seen_with(Method::Delete)
            .into_iter()
            .map(|seen| seen.url)
            .collect();
        assert_eq!(deleted, [SKIN, CAPE]);
        assert!(matches!(
            site.set_cape(&cx, Some("cape9")).await,
            Err(PlayerError::Unsupported {
                operation: "choose a cape",
                ..
            })
        ));
    }
}
