use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use coblet_auth::SkinModel;
use coblet_runtime::{Clock, HttpUrl, Storage, SystemClock};
use coblet_types::PlayerName;
use serde::{Deserialize, Serialize};

use crate::{MinecraftServices, PlayerError, check_skin_png};

const INDEX: &str = "index.json";
const FORMAT: u32 = 1;
const MAX_NAME: usize = 64;

/// Where a library skin came from.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkinOrigin {
    /// A file the user picked.
    File,
    /// Downloaded from a URL.
    Url(HttpUrl),
    /// Copied from a player's public skin.
    Player(PlayerName),
}

/// Skin saved in the local library.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibrarySkin {
    /// Library ID, also the file name without `.png`.
    pub id: String,
    /// Name shown to the user.
    pub name: String,
    /// Arm model the skin is drawn for.
    pub model: SkinModel,
    /// Where it came from.
    pub origin: SkinOrigin,
    /// When it was added.
    pub added_at: SystemTime,
}

/// Local skin library in `data_root/skins/`: PNG files plus an index with
/// names, models and origins.
///
/// Offline accounts cannot upload skins, but a launcher can still show and
/// manage them here; the engine only provides PNG data and the model, the
/// UI draws previews.
pub struct SkinLibrary {
    storage: Arc<dyn Storage>,
    player: MinecraftServices,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    writer: tokio::sync::Mutex<()>,
}

#[derive(Serialize, Deserialize)]
struct RawIndex {
    format: u32,
    #[serde(default)]
    skins: Vec<RawSkin>,
}

#[derive(Serialize, Deserialize)]
struct RawSkin {
    id: String,
    name: String,
    model: String,
    origin: RawOrigin,
    added_at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RawOrigin {
    File,
    Url { url: HttpUrl },
    Player { name: PlayerName },
}

impl SkinLibrary {
    /// Opens the library in `dir`; nothing is created before the first import.
    #[must_use]
    pub fn new(
        storage: Arc<dyn Storage>,
        player: MinecraftServices,
        dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            storage,
            player,
            dir: dir.into(),
            clock: Arc::new(SystemClock),
            writer: tokio::sync::Mutex::new(()),
        }
    }

    /// Uses this clock for `added_at`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Returns the library directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the PNG path of a skin.
    #[must_use]
    pub fn png_path(&self, skin: &LibrarySkin) -> PathBuf {
        self.dir.join(format!("{}.png", skin.id))
    }

    /// Lists the skins in the order they were added.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Library`] for a damaged index.
    pub async fn list(&self) -> Result<Vec<LibrarySkin>, PlayerError> {
        self.load().await
    }

    /// Returns one skin.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::UnknownSkin`] for an unknown ID.
    pub async fn get(&self, id: &str) -> Result<LibrarySkin, PlayerError> {
        self.load()
            .await?
            .into_iter()
            .find(|skin| skin.id == id)
            .ok_or_else(|| PlayerError::UnknownSkin(id.to_owned()))
    }

    /// Reads the PNG of a skin.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::UnknownSkin`] or a storage error.
    pub async fn png(&self, id: &str) -> Result<Vec<u8>, PlayerError> {
        let skin = self.get(id).await?;
        Ok(self.storage.read(&self.png_path(&skin)).await?.to_vec())
    }

    /// Adds a skin from PNG bytes.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::InvalidSkin`] when the bytes are not a skin.
    pub async fn import_file(
        &self,
        name: &str,
        png: Vec<u8>,
        model: SkinModel,
    ) -> Result<LibrarySkin, PlayerError> {
        self.add(name, png, model, SkinOrigin::File).await
    }

    /// Downloads a skin from a URL and adds it.
    ///
    /// # Errors
    ///
    /// Returns a download error or [`PlayerError::InvalidSkin`].
    pub async fn import_url(
        &self,
        name: &str,
        url: HttpUrl,
        model: SkinModel,
    ) -> Result<LibrarySkin, PlayerError> {
        let png = self.player.download(&url).await?;
        self.add(name, png, model, SkinOrigin::Url(url)).await
    }

    /// Copies a player's current public skin, named after the player.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::UnknownPlayer`] for an unknown name and
    /// [`PlayerError::NoSkin`] when the player wears the default skin.
    pub async fn import_user(&self, player: &PlayerName) -> Result<LibrarySkin, PlayerError> {
        let public = self.player.lookup(player).await?;
        let url = public.skin.ok_or(PlayerError::NoSkin)?;
        let png = self.player.download(&url).await?;
        let name = public.name.to_string();
        self.add(&name, png, public.model, SkinOrigin::Player(public.name))
            .await
    }

    /// Renames a skin.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::UnknownSkin`] or [`PlayerError::Library`] for
    /// an empty or too long name.
    pub async fn rename(&self, id: &str, name: &str) -> Result<LibrarySkin, PlayerError> {
        let name = clean_name(name)?;
        let _writer = self.writer.lock().await;
        let mut skins = self.load().await?;
        let skin = skins
            .iter_mut()
            .find(|skin| skin.id == id)
            .ok_or_else(|| PlayerError::UnknownSkin(id.to_owned()))?;
        skin.name = name;
        let renamed = skin.clone();
        self.save(&skins).await?;
        Ok(renamed)
    }

    /// Removes a skin and its file.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::UnknownSkin`] or a storage error.
    pub async fn remove(&self, id: &str) -> Result<(), PlayerError> {
        let _writer = self.writer.lock().await;
        let mut skins = self.load().await?;
        let index = skins
            .iter()
            .position(|skin| skin.id == id)
            .ok_or_else(|| PlayerError::UnknownSkin(id.to_owned()))?;
        let skin = skins.remove(index);
        self.save(&skins).await?;
        match self.storage.remove_file(&self.png_path(&skin)).await {
            Err(error) if !error.is_not_found() => Err(error.into()),
            _ => Ok(()),
        }
    }

    async fn add(
        &self,
        name: &str,
        png: Vec<u8>,
        model: SkinModel,
        origin: SkinOrigin,
    ) -> Result<LibrarySkin, PlayerError> {
        let name = clean_name(name)?;
        check_skin_png(&png)?;
        let skin = LibrarySkin {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name,
            model,
            origin,
            added_at: self.clock.now(),
        };
        let _writer = self.writer.lock().await;
        let mut skins = self.load().await?;
        self.storage.create_dir_all(&self.dir).await?;
        self.storage
            .write_atomic(&self.png_path(&skin), png.into())
            .await?;
        skins.push(skin.clone());
        self.save(&skins).await?;
        Ok(skin)
    }

    async fn load(&self) -> Result<Vec<LibrarySkin>, PlayerError> {
        let bytes = match self.storage.read(&self.dir.join(INDEX)).await {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let raw: RawIndex = serde_json::from_slice(&bytes)
            .map_err(|error| PlayerError::Library(error.to_string()))?;
        if raw.format != FORMAT {
            return Err(PlayerError::Library(format!(
                "format {} is not supported, only {FORMAT} is",
                raw.format
            )));
        }
        raw.skins.into_iter().map(from_raw).collect()
    }

    async fn save(&self, skins: &[LibrarySkin]) -> Result<(), PlayerError> {
        let raw = RawIndex {
            format: FORMAT,
            skins: skins.iter().map(to_raw).collect(),
        };
        let mut json = serde_json::to_vec_pretty(&raw)
            .map_err(|error| PlayerError::Library(error.to_string()))?;
        json.push(b'\n');
        self.storage.create_dir_all(&self.dir).await?;
        self.storage
            .write_atomic(&self.dir.join(INDEX), json.into())
            .await?;
        Ok(())
    }
}

impl std::fmt::Debug for SkinLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkinLibrary")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn clean_name(name: &str) -> Result<String, PlayerError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(PlayerError::Library(format!(
            "skin name {name:?} must be 1 to {MAX_NAME} printable characters"
        )));
    }
    Ok(name.to_owned())
}

fn to_raw(skin: &LibrarySkin) -> RawSkin {
    RawSkin {
        id: skin.id.clone(),
        name: skin.name.clone(),
        model: skin.model.as_str().to_owned(),
        origin: match &skin.origin {
            SkinOrigin::File => RawOrigin::File,
            SkinOrigin::Url(url) => RawOrigin::Url { url: url.clone() },
            SkinOrigin::Player(name) => RawOrigin::Player { name: name.clone() },
        },
        added_at: skin
            .added_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    }
}

fn from_raw(raw: RawSkin) -> Result<LibrarySkin, PlayerError> {
    if raw.id.is_empty() || !raw.id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(PlayerError::Library(format!(
            "invalid skin id {:?}",
            raw.id
        )));
    }
    Ok(LibrarySkin {
        id: raw.id,
        name: raw.name,
        model: SkinModel::parse(&raw.model).unwrap_or_default(),
        origin: match raw.origin {
            RawOrigin::File => SkinOrigin::File,
            RawOrigin::Url { url } => SkinOrigin::Url(url),
            RawOrigin::Player { name } => SkinOrigin::Player(name),
        },
        added_at: SystemTime::UNIX_EPOCH + Duration::from_secs(raw.added_at),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use coblet_auth::SkinModel;
    use coblet_runtime::{HttpUrl, Method, TokioStorage};
    use coblet_types::PlayerName;

    use super::{SkinLibrary, SkinOrigin};
    use crate::mock::Mock;
    use crate::png::sample;
    use crate::{MinecraftServices, PlayerError};

    fn library(mock: &Arc<Mock>, dir: &tempfile::TempDir) -> SkinLibrary {
        SkinLibrary::new(
            Arc::new(TokioStorage::new()),
            MinecraftServices::new(mock.clone()),
            dir.path().join("skins"),
        )
    }

    #[tokio::test]
    async fn imports_renames_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new();
        mock.on(
            Method::Get,
            "https://example.com/a.png",
            200,
            sample(64, 32),
        );
        let library = library(&mock, &dir);
        assert_eq!(library.list().await.unwrap().len(), 0);

        let file = library
            .import_file(" Knight ", sample(64, 64), SkinModel::Slim)
            .await
            .unwrap();
        assert_eq!(file.name, "Knight");
        let url = HttpUrl::new("https://example.com/a.png").unwrap();
        let downloaded = library
            .import_url("Old", url.clone(), SkinModel::Classic)
            .await
            .unwrap();
        assert_eq!(downloaded.origin, SkinOrigin::Url(url));
        assert_eq!(library.png(&downloaded.id).await.unwrap(), sample(64, 32));

        library.rename(&file.id, "Paladin").await.unwrap();
        let listed = library.list().await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "Paladin");
        assert_eq!(listed[0].model, SkinModel::Slim);

        library.remove(&file.id).await.unwrap();
        assert!(!library.png_path(&file).exists());
        assert!(matches!(
            library.get(&file.id).await,
            Err(PlayerError::UnknownSkin(_))
        ));
        assert!(matches!(
            library.rename(&downloaded.id, "  ").await,
            Err(PlayerError::Library(_))
        ));
        assert!(matches!(
            library
                .import_file("Bad", sample(16, 16), SkinModel::Classic)
                .await,
            Err(PlayerError::InvalidSkin(_))
        ));
        assert_eq!(library.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn copies_a_players_skin() {
        let dir = tempfile::tempdir().unwrap();
        let textures = STANDARD.encode(
            br#"{"textures": {"SKIN": {"url": "http://textures.minecraft.net/texture/s", "metadata": {"model": "slim"}}}}"#,
        );
        let mock = Mock::new();
        mock.on(
            Method::Get,
            "https://api.mojang.com/users/profiles/minecraft/jeb_",
            200,
            r#"{"id": "853c80ef3c3749fdaa49938b674adae6", "name": "jeb_"}"#,
        )
        .on(
            Method::Get,
            "https://sessionserver.mojang.com/session/minecraft/profile/853c80ef3c3749fdaa49938b674adae6",
            200,
            format!(
                r#"{{"id": "853c80ef3c3749fdaa49938b674adae6", "name": "jeb_",
                    "properties": [{{"name": "textures", "value": "{textures}"}}]}}"#
            ),
        )
        .on(
            Method::Get,
            "https://textures.minecraft.net/texture/s",
            200,
            sample(64, 64),
        );
        let library = library(&mock, &dir);
        let skin = library
            .import_user(&PlayerName::new("jeb_").unwrap())
            .await
            .unwrap();
        assert_eq!(skin.name, "jeb_");
        assert_eq!(skin.model, SkinModel::Slim);
        assert!(matches!(skin.origin, SkinOrigin::Player(ref name) if name.as_str() == "jeb_"));
    }
}
