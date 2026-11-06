use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coblet_auth::SkinModel;
use coblet_runtime::{HttpUrl, Method, TokioStorage};
use coblet_types::PlayerName;

use super::{SkinLibrary, SkinOrigin};
use crate::mock::Mock;
use crate::png::sample;
use crate::{Player, PlayerError};

fn library(mock: &Arc<Mock>, dir: &tempfile::TempDir) -> SkinLibrary {
    SkinLibrary::new(
        Arc::new(TokioStorage::new()),
        Player::new(mock.clone()),
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
