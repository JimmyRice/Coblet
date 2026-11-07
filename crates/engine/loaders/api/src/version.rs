use std::collections::BTreeMap;

use coblet_catalog::{CatalogItem, Timestamp};
use coblet_types::{GameVersionId, VersionType};
use serde::{Deserialize, Serialize};

/// Version of a Mod Loader for one game version, as listed in a version picker.
///
/// The Mod Loader sets [`recommended`](LoaderVersion::recommended) by its
/// own rule, such as the latest stable build, so the picker's star needs no
/// knowledge of the loader. [`data`](LoaderVersion::data) carries what the
/// loader needs later to install this version, such as an installer address.
///
/// # Examples
///
/// ```
/// use coblet_catalog::CatalogItem;
/// use coblet_loader::LoaderVersion;
/// use coblet_types::{GameVersionId, VersionType};
///
/// let game: GameVersionId = "1.20.1".parse()?;
/// let version = LoaderVersion::new("0.16.9").for_game(game.clone()).recommended();
/// assert!(version.is_recommended());
/// assert!(version.is_compatible_with(&game));
/// assert_eq!(version.version_type(), &VersionType::Release);
/// # Ok::<(), coblet_types::GameVersionIdError>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderVersion {
    /// Version string, opaque like every version string.
    pub id: String,
    /// Game version the entry is for; `None` when it fits every version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<GameVersionId>,
    /// Release channel.
    #[serde(rename = "type", default = "release")]
    pub version_type: VersionType,
    /// Release time, when the source knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<Timestamp>,
    /// Whether the Mod Loader recommends this version for the game version.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recommended: bool,
    /// Data of the Mod Loader for this version, kept verbatim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, serde_json::Value>,
}

fn release() -> VersionType {
    VersionType::Release
}

impl LoaderVersion {
    /// Creates a release version that fits every game version.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            game: None,
            version_type: VersionType::Release,
            released: None,
            recommended: false,
            data: BTreeMap::new(),
        }
    }

    /// Ties the version to one game version.
    #[must_use]
    pub fn for_game(self, game: GameVersionId) -> Self {
        Self {
            game: Some(game),
            ..self
        }
    }

    /// Sets the release channel.
    #[must_use]
    pub fn with_type(self, version_type: VersionType) -> Self {
        Self {
            version_type,
            ..self
        }
    }

    /// Sets the release time.
    #[must_use]
    pub fn released_at(self, time: Timestamp) -> Self {
        Self {
            released: Some(time),
            ..self
        }
    }

    /// Marks the version as recommended.
    #[must_use]
    pub fn recommended(self) -> Self {
        Self {
            recommended: true,
            ..self
        }
    }

    /// Adds a data entry.
    #[must_use]
    pub fn with_data(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.data.insert(key.into(), value);
        self
    }
}

impl CatalogItem for LoaderVersion {
    fn key(&self) -> &str {
        &self.id
    }

    fn version_type(&self) -> &VersionType {
        &self.version_type
    }

    fn released(&self) -> Option<Timestamp> {
        self.released
    }

    fn is_recommended(&self) -> bool {
        self.recommended
    }

    fn is_compatible_with(&self, game: &GameVersionId) -> bool {
        self.game.as_ref().is_none_or(|own| own == game)
    }
}

#[cfg(test)]
mod tests {
    use coblet_types::VersionType;

    use super::LoaderVersion;

    #[test]
    fn serializes_compactly_and_defaults_to_release() {
        let version: LoaderVersion = serde_json::from_str(r#"{ "id": "1.0" }"#).unwrap();
        assert_eq!(version.version_type, VersionType::Release);
        assert_eq!(
            serde_json::to_string(&version).unwrap(),
            r#"{"id":"1.0","type":"release"}"#
        );
        let beta = LoaderVersion::new("2.0-beta")
            .with_type(VersionType::Other("beta".to_owned()))
            .with_data(
                "installer",
                serde_json::json!("https://maven.example/x.jar"),
            );
        let json = serde_json::to_string(&beta).unwrap();
        assert_eq!(serde_json::from_str::<LoaderVersion>(&json).unwrap(), beta);
    }
}
