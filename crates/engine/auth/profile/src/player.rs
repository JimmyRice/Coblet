use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coblet_auth::{PlayerProfile, SkinModel};
use coblet_launch_command::Credential;
use coblet_runtime::{HttpUrl, Method, Request, Transport};
use coblet_types::PlayerName;
use serde::Deserialize;
use serde_json::json;

use crate::http::{Reply, send, url};
use crate::{PlayerError, check_skin_png};

const PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";
const NAME_LOOKUP: &str = "https://api.mojang.com/users/profiles/minecraft";
const SESSION_PROFILE: &str = "https://sessionserver.mojang.com/session/minecraft/profile";

/// Skin to upload.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkinSource {
    /// PNG bytes, checked with [`check_skin_png`] before upload.
    Png(Vec<u8>),
    /// Public URL Minecraft services download the skin from.
    Url(HttpUrl),
}

/// Skin and cape of a player as anyone can see them.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicPlayer {
    /// Player name with its current spelling.
    pub name: PlayerName,
    /// Profile UUID without dashes.
    pub uuid: String,
    /// Skin URL, `None` for the default skin.
    pub skin: Option<HttpUrl>,
    /// Arm model of the skin.
    pub model: SkinModel,
    /// Cape URL, if the player shows one.
    pub cape: Option<HttpUrl>,
}

/// Skins and capes on Minecraft services.
///
/// Calls take the Minecraft access token of an online session, for example
/// [`LaunchSession::access_token`](coblet_auth::LaunchSession::access_token).
/// Changes return the updated profile, which the caller can save with
/// [`AccountStore::set_profile`](coblet_auth::AccountStore::set_profile).
/// [`PlayerError::Unauthorized`] means the token expired: refresh the
/// account and repeat the call.
#[derive(Clone)]
pub struct Player {
    transport: Arc<dyn Transport>,
}

#[derive(Deserialize)]
struct RawLookup {
    id: String,
}

#[derive(Deserialize)]
struct RawSessionProfile {
    id: String,
    name: String,
    #[serde(default)]
    properties: Vec<RawProperty>,
}

#[derive(Deserialize)]
struct RawProperty {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct RawTextures {
    #[serde(default)]
    textures: RawTextureSet,
}

#[derive(Default, Deserialize)]
struct RawTextureSet {
    #[serde(rename = "SKIN")]
    skin: Option<RawTexture>,
    #[serde(rename = "CAPE")]
    cape: Option<RawTexture>,
}

#[derive(Deserialize)]
struct RawTexture {
    url: String,
    metadata: Option<RawMetadata>,
}

#[derive(Deserialize)]
struct RawMetadata {
    model: Option<String>,
}

impl Player {
    /// Creates the service.
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self { transport }
    }

    /// Returns the profile with all skins and capes and which are active.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Unauthorized`] for an expired token and
    /// other errors for failed requests.
    pub async fn appearance(&self, token: &Credential) -> Result<PlayerProfile, PlayerError> {
        let reply = self
            .call("profile", Method::Get, PROFILE, token, None)
            .await?;
        profile(&reply)
    }

    /// Uploads a new skin and makes it active.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::InvalidSkin`] when PNG bytes are not a skin,
    /// before anything is sent.
    pub async fn upload_skin(
        &self,
        token: &Credential,
        source: SkinSource,
        model: SkinModel,
    ) -> Result<PlayerProfile, PlayerError> {
        let endpoint = format!("{PROFILE}/skins");
        let body = match source {
            SkinSource::Png(png) => {
                check_skin_png(&png)?;
                let boundary = format!("coblet-{}", uuid::Uuid::new_v4().simple());
                Body::Multipart(boundary.clone(), multipart(&boundary, model, &png))
            }
            SkinSource::Url(skin) => {
                Body::Json(json!({ "variant": model.as_str(), "url": skin.as_str() }))
            }
        };
        let reply = self
            .call("skins", Method::Post, &endpoint, token, Some(body))
            .await?;
        profile(&reply)
    }

    /// Switches the arm model by submitting the active skin again.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::NoSkin`] when the player wears the default skin.
    pub async fn set_model(
        &self,
        token: &Credential,
        model: SkinModel,
    ) -> Result<PlayerProfile, PlayerError> {
        let current = self.appearance(token).await?;
        let skin = current.active_skin().ok_or(PlayerError::NoSkin)?;
        if skin.model == model {
            return Ok(current);
        }
        self.upload_skin(token, SkinSource::Url(skin.url.clone()), model)
            .await
    }

    /// Goes back to the default skin.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails.
    pub async fn reset_skin(&self, token: &Credential) -> Result<PlayerProfile, PlayerError> {
        let endpoint = format!("{PROFILE}/skins/active");
        let reply = self
            .call("skins", Method::Delete, &endpoint, token, None)
            .await?;
        profile(&reply)
    }

    /// Shows one of the account's capes, or hides the cape with `None`.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::CapeNotOwned`] when the account does not own
    /// the cape; nothing is changed then.
    pub async fn set_cape(
        &self,
        token: &Credential,
        cape: Option<&str>,
    ) -> Result<PlayerProfile, PlayerError> {
        let endpoint = format!("{PROFILE}/capes/active");
        let reply = match cape {
            Some(id) => {
                let current = self.appearance(token).await?;
                if !current.capes.iter().any(|owned| owned.id == id) {
                    return Err(PlayerError::CapeNotOwned(id.to_owned()));
                }
                let body = Body::Json(json!({ "capeId": id }));
                self.call("capes", Method::Put, &endpoint, token, Some(body))
                    .await?
            }
            None => {
                self.call("capes", Method::Delete, &endpoint, token, None)
                    .await?
            }
        };
        profile(&reply)
    }

    /// Looks up a player's public skin and cape by name.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::UnknownPlayer`] when no player has this name.
    pub async fn lookup(&self, name: &PlayerName) -> Result<PublicPlayer, PlayerError> {
        let request = Request::get(url(&format!("{NAME_LOOKUP}/{name}"))?);
        let reply = send(&self.transport, "name lookup", request, None).await?;
        if matches!(reply.status, 204 | 404) {
            return Err(PlayerError::UnknownPlayer(name.to_string()));
        }
        if !reply.is_success() {
            return Err(reply.error());
        }
        let found: RawLookup = reply.json()?;
        let request = Request::get(url(&format!("{SESSION_PROFILE}/{}", found.id))?);
        let reply = send(&self.transport, "session profile", request, None).await?;
        if matches!(reply.status, 204 | 404) {
            return Err(PlayerError::UnknownPlayer(name.to_string()));
        }
        if !reply.is_success() {
            return Err(reply.error());
        }
        public_player(&reply)
    }

    /// Downloads a texture such as a skin or cape PNG.
    ///
    /// Minecraft services hand out texture URLs on
    /// `http://textures.minecraft.net`; they are fetched over HTTPS instead.
    ///
    /// # Errors
    ///
    /// Returns an error when the download fails or is larger than 256 KiB.
    pub async fn download(&self, texture: &HttpUrl) -> Result<Vec<u8>, PlayerError> {
        let request = Request::get(secure_texture(texture));
        let limit = u64::try_from(crate::MAX_SKIN_BYTES).unwrap_or(u64::MAX);
        let reply = send(&self.transport, "texture", request, Some(limit)).await?;
        if !reply.is_success() {
            return Err(reply.error());
        }
        Ok(reply.body)
    }

    async fn call(
        &self,
        endpoint: &'static str,
        method: Method,
        target: &str,
        token: &Credential,
        body: Option<Body>,
    ) -> Result<Reply, PlayerError> {
        let mut request = Request::new(method, url(target)?)
            .header("Authorization", format!("Bearer {}", token.expose()))
            .header("Accept", "application/json");
        match body {
            Some(Body::Json(json)) => {
                request = request
                    .header("Content-Type", "application/json")
                    .body(json.to_string());
            }
            Some(Body::Multipart(boundary, bytes)) => {
                request = request
                    .header(
                        "Content-Type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(bytes);
            }
            None => {}
        }
        let reply = send(&self.transport, endpoint, request, None).await?;
        if reply.is_success() {
            Ok(reply)
        } else {
            Err(reply.error())
        }
    }
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

enum Body {
    Json(serde_json::Value),
    Multipart(String, Vec<u8>),
}

fn multipart(boundary: &str, model: SkinModel, png: &[u8]) -> Vec<u8> {
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"variant\"\r\n\r\n{model}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skin.png\"\r\n\
         Content-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn profile(reply: &Reply) -> Result<PlayerProfile, PlayerError> {
    PlayerProfile::from_services_json(&reply.body).map_err(|error| PlayerError::InvalidResponse {
        endpoint: reply.endpoint,
        message: error.to_string(),
    })
}

fn public_player(reply: &Reply) -> Result<PublicPlayer, PlayerError> {
    let invalid = |message: String| PlayerError::InvalidResponse {
        endpoint: reply.endpoint,
        message,
    };
    let raw: RawSessionProfile = reply.json()?;
    let name = PlayerName::new(raw.name).map_err(|error| invalid(error.to_string()))?;
    let identity = PlayerProfile::new(name, &raw.id).map_err(|error| invalid(error.to_string()))?;
    let textures = match raw.properties.iter().find(|p| p.name == "textures") {
        Some(property) => {
            let decoded = STANDARD
                .decode(property.value.as_bytes())
                .map_err(|error| invalid(error.to_string()))?;
            serde_json::from_slice::<RawTextures>(&decoded)
                .map_err(|error| invalid(error.to_string()))?
                .textures
        }
        None => RawTextureSet::default(),
    };
    let model = textures
        .skin
        .as_ref()
        .and_then(|skin| skin.metadata.as_ref())
        .and_then(|metadata| metadata.model.as_deref())
        .and_then(SkinModel::parse)
        .unwrap_or_default();
    let texture = |texture: Option<RawTexture>| {
        texture
            .map(|texture| HttpUrl::new(texture.url).map_err(|error| invalid(error.to_string())))
            .transpose()
    };
    Ok(PublicPlayer {
        name: identity.name,
        uuid: identity.uuid,
        skin: texture(textures.skin)?,
        model,
        cape: texture(textures.cape)?,
    })
}


/// Moves texture URLs of `textures.minecraft.net` from HTTP to HTTPS.
fn secure_texture(url: &HttpUrl) -> HttpUrl {
    url.as_str()
        .strip_prefix("http://textures.minecraft.net/")
        .and_then(|path| HttpUrl::new(format!("https://textures.minecraft.net/{path}")).ok())
        .unwrap_or_else(|| url.clone())
}

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;
