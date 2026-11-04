use std::collections::BTreeMap;

use coblet_types::Sha1Digest;
use serde::{Deserialize, Serialize};

/// Base address of asset objects.
pub const RESOURCES_URL: &str = "https://resources.download.minecraft.net/";

/// Parsed asset index (`assets/indexes/<id>.json`).
///
/// Objects are stored by hash under `assets/objects/<first two hex digits>/<hash>`.
/// Old indexes also need the objects copied to their names: `virtual`
/// indexes (`legacy`) into `assets/virtual/<id>/`, and `map_to_resources`
/// indexes (`pre-1.6`) into the game directory's `resources/`.
///
/// # Examples
///
/// ```
/// use coblet_mojang::AssetIndex;
///
/// let index = AssetIndex::from_slice(br#"{"map_to_resources": true, "objects": {
///     "sound/step/grass1.ogg": {"hash": "227ab99bf7c6cf0b2002e0f7957d0ff7e5cb0c96", "size": 5390}
/// }}"#)?;
/// let (name, object) = index.objects.iter().next().unwrap();
/// assert_eq!(name, "sound/step/grass1.ogg");
/// assert_eq!(object.path(), "22/227ab99bf7c6cf0b2002e0f7957d0ff7e5cb0c96");
/// assert!(index.map_to_resources);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetIndex {
    /// Objects by their name in the game, such as `minecraft/sounds/ambient/cave/cave1.ogg`.
    pub objects: BTreeMap<String, AssetObject>,
    /// Copy objects into `assets/virtual/<id>/` by name.
    #[serde(
        rename = "virtual",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_virtual: bool,
    /// Copy objects into the game directory's `resources/` by name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub map_to_resources: bool,
}

impl AssetIndex {
    /// Parses an asset index.
    ///
    /// # Errors
    ///
    /// Returns the [`serde_json::Error`] of malformed documents.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Returns the total size of all objects.
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.objects.values().map(|object| object.size).sum()
    }
}

/// One asset object.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetObject {
    /// SHA-1 of the object, which is also its storage key.
    pub hash: Sha1Digest,
    /// Size in bytes.
    pub size: u64,
}

impl AssetObject {
    /// Returns the path under `assets/objects/`, such as `22/227ab9…`.
    #[must_use]
    pub fn path(&self) -> String {
        let hash = self.hash.to_string();
        format!("{}/{hash}", &hash[..2])
    }

    /// Returns the download address.
    #[must_use]
    pub fn url(&self) -> String {
        format!("{RESOURCES_URL}{}", self.path())
    }
}
