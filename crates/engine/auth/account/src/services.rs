use coblet_runtime::HttpUrl;
use coblet_types::PlayerName;
use serde::Deserialize;

use crate::{Cape, PlayerProfile, Skin, SkinModel};

/// Reason a Minecraft services profile document was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid minecraft profile: {0}")]
pub struct ProfileJsonError(String);

#[derive(Deserialize)]
struct RawProfile {
    id: String,
    name: String,
    #[serde(default)]
    skins: Vec<RawSkin>,
    #[serde(default)]
    capes: Vec<RawCape>,
}

#[derive(Deserialize)]
struct RawSkin {
    id: String,
    #[serde(default)]
    state: String,
    url: String,
    #[serde(default)]
    variant: String,
}

#[derive(Deserialize)]
struct RawCape {
    id: String,
    #[serde(default)]
    state: String,
    url: String,
    alias: Option<String>,
}

impl PlayerProfile {
    /// Parses the body of `GET https://api.minecraftservices.com/minecraft/profile`.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileJsonError`] when the document is not a profile or
    /// carries an invalid name, UUID or texture URL.
    ///
    /// # Examples
    ///
    /// ```
    /// use coblet_auth::{PlayerProfile, SkinModel};
    ///
    /// let profile = PlayerProfile::from_services_json(br#"{
    ///     "id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch",
    ///     "skins": [{"id": "s1", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/a", "variant": "SLIM"}],
    ///     "capes": [{"id": "c1", "state": "INACTIVE", "url": "http://textures.minecraft.net/texture/b", "alias": "Migrator"}]
    /// }"#)?;
    /// assert_eq!(profile.active_skin().unwrap().model, SkinModel::Slim);
    /// assert!(profile.active_cape().is_none());
    /// # Ok::<(), coblet_auth::ProfileJsonError>(())
    /// ```
    pub fn from_services_json(json: &[u8]) -> Result<Self, ProfileJsonError> {
        let raw: RawProfile =
            serde_json::from_slice(json).map_err(|error| ProfileJsonError(error.to_string()))?;
        let name =
            PlayerName::new(raw.name).map_err(|error| ProfileJsonError(error.to_string()))?;
        let mut profile =
            Self::new(name, &raw.id).map_err(|error| ProfileJsonError(error.to_string()))?;
        for skin in raw.skins {
            profile.skins.push(Skin::new(
                skin.id,
                texture_url(skin.url)?,
                SkinModel::parse(&skin.variant).unwrap_or_default(),
                is_active(&skin.state),
            ));
        }
        for cape in raw.capes {
            profile.capes.push(Cape::new(
                cape.id,
                cape.alias,
                texture_url(cape.url)?,
                is_active(&cape.state),
            ));
        }
        Ok(profile)
    }
}

fn texture_url(url: String) -> Result<HttpUrl, ProfileJsonError> {
    HttpUrl::new(url).map_err(|error| ProfileJsonError(error.to_string()))
}

fn is_active(state: &str) -> bool {
    state.eq_ignore_ascii_case("active")
}

#[cfg(test)]
mod tests {
    use crate::PlayerProfile;

    #[test]
    fn rejects_documents_without_a_valid_identity() {
        assert!(PlayerProfile::from_services_json(b"{}").is_err());
        assert!(PlayerProfile::from_services_json(br#"{"id": "short", "name": "Notch"}"#).is_err());
        assert!(
            PlayerProfile::from_services_json(
                br#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "bad name"}"#
            )
            .is_err()
        );
        let bare = PlayerProfile::from_services_json(
            br#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch"}"#,
        )
        .unwrap();
        assert!(bare.skins.is_empty() && bare.capes.is_empty());
    }
}
