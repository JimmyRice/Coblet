use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coblet_auth::SkinModel;
use coblet_launch_command::Credential;
use coblet_runtime::{HttpUrl, Method};
use coblet_types::PlayerName;

use super::{Player, SkinSource};
use crate::PlayerError;
use crate::mock::Mock;
use crate::png::sample;

const PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";
const SKINS: &str = "https://api.minecraftservices.com/minecraft/profile/skins";
const ACTIVE_SKIN: &str = "https://api.minecraftservices.com/minecraft/profile/skins/active";
const ACTIVE_CAPE: &str = "https://api.minecraftservices.com/minecraft/profile/capes/active";

fn profile_json(model: &str, cape_active: bool) -> String {
    let state = if cape_active { "ACTIVE" } else { "INACTIVE" };
    format!(
        r#"{{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch",
            "skins": [{{"id": "s1", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/abc", "variant": "{model}"}}],
            "capes": [{{"id": "cape-1", "state": "{state}", "url": "http://textures.minecraft.net/texture/c", "alias": "Migrator"}}]}}"#
    )
}

fn token() -> Credential {
    Credential::new("mc-token")
}

#[tokio::test]
async fn reads_the_appearance_with_the_token() {
    let mock = Mock::new();
    mock.on(Method::Get, PROFILE, 200, profile_json("CLASSIC", true));
    let profile = Player::new(mock.clone())
        .appearance(&token())
        .await
        .unwrap();
    assert_eq!(
        profile.active_cape().unwrap().alias.as_deref(),
        Some("Migrator")
    );
    assert_eq!(
        mock.seen()[0].header("authorization"),
        Some("Bearer mc-token")
    );

    let mock = Mock::new();
    mock.on(Method::Get, PROFILE, 401, "");
    assert!(matches!(
        Player::new(mock).appearance(&token()).await,
        Err(PlayerError::Unauthorized)
    ));
}

#[tokio::test]
async fn uploads_skins_from_files_and_urls() {
    let mock = Mock::new();
    mock.on(Method::Post, SKINS, 200, profile_json("SLIM", false));
    let player = Player::new(mock.clone());
    let png = sample(64, 64);
    let profile = player
        .upload_skin(&token(), SkinSource::Png(png.clone()), SkinModel::Slim)
        .await
        .unwrap();
    assert_eq!(profile.active_skin().unwrap().model, SkinModel::Slim);
    let upload = &mock.seen()[0];
    let content_type = upload.header("content-type").unwrap();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    let text = upload.text();
    assert!(text.starts_with(&format!("--{boundary}\r\n")));
    assert!(text.contains("name=\"variant\"\r\n\r\nslim\r\n"));
    assert!(text.contains("filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n"));
    assert!(text.ends_with(&format!("\r\n--{boundary}--\r\n")));
    assert!(upload.body.windows(png.len()).any(|window| window == png));

    let skin = HttpUrl::new("https://example.com/skin.png").unwrap();
    player
        .upload_skin(&token(), SkinSource::Url(skin), SkinModel::Classic)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&mock.seen()[1].body).unwrap();
    assert_eq!(json["variant"], "classic");
    assert_eq!(json["url"], "https://example.com/skin.png");

    assert!(matches!(
        player
            .upload_skin(
                &token(),
                SkinSource::Png(sample(32, 32)),
                SkinModel::Classic
            )
            .await,
        Err(PlayerError::InvalidSkin(_))
    ));
    assert_eq!(mock.seen().len(), 2);
}

#[tokio::test]
async fn switches_the_model_by_resubmitting_the_skin() {
    let mock = Mock::new();
    mock.on(Method::Get, PROFILE, 200, profile_json("CLASSIC", false))
        .on(Method::Post, SKINS, 200, profile_json("SLIM", false));
    let player = Player::new(mock.clone());
    let profile = player.set_model(&token(), SkinModel::Slim).await.unwrap();
    assert_eq!(profile.active_skin().unwrap().model, SkinModel::Slim);
    let post = &mock.seen_with(Method::Post)[0];
    let json: serde_json::Value = serde_json::from_slice(&post.body).unwrap();
    assert_eq!(json["url"], "http://textures.minecraft.net/texture/abc");
    assert_eq!(json["variant"], "slim");

    player
        .set_model(&token(), SkinModel::Classic)
        .await
        .unwrap();
    assert_eq!(mock.seen_with(Method::Post).len(), 1);

    let mock = Mock::new();
    mock.on(
        Method::Get,
        PROFILE,
        200,
        r#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch", "skins": [], "capes": []}"#,
    );
    assert!(matches!(
        Player::new(mock).set_model(&token(), SkinModel::Slim).await,
        Err(PlayerError::NoSkin)
    ));
}

#[tokio::test]
async fn resets_skins_and_switches_capes() {
    let mock = Mock::new();
    mock.on(
        Method::Delete,
        ACTIVE_SKIN,
        200,
        profile_json("CLASSIC", false),
    )
    .on(Method::Get, PROFILE, 200, profile_json("CLASSIC", false))
    .on(Method::Put, ACTIVE_CAPE, 200, profile_json("CLASSIC", true))
    .on(
        Method::Delete,
        ACTIVE_CAPE,
        200,
        profile_json("CLASSIC", false),
    );
    let player = Player::new(mock.clone());
    player.reset_skin(&token()).await.unwrap();
    assert_eq!(mock.seen_with(Method::Delete)[0].url, ACTIVE_SKIN);

    assert!(matches!(
        player.set_cape(&token(), Some("not-mine")).await,
        Err(PlayerError::CapeNotOwned(id)) if id == "not-mine"
    ));
    assert!(mock.seen_with(Method::Put).is_empty());

    let shown = player.set_cape(&token(), Some("cape-1")).await.unwrap();
    assert!(shown.active_cape().is_some());
    let json: serde_json::Value =
        serde_json::from_slice(&mock.seen_with(Method::Put)[0].body).unwrap();
    assert_eq!(json["capeId"], "cape-1");

    let hidden = player.set_cape(&token(), None).await.unwrap();
    assert!(hidden.active_cape().is_none());
    assert_eq!(mock.seen_with(Method::Delete)[1].url, ACTIVE_CAPE);
}

#[tokio::test]
async fn looks_up_public_skins_by_name() {
    let textures = STANDARD.encode(
        br#"{"textures": {"SKIN": {"url": "http://textures.minecraft.net/texture/s", "metadata": {"model": "slim"}},
                          "CAPE": {"url": "http://textures.minecraft.net/texture/c"}}}"#,
    );
    let mock = Mock::new();
    mock.on(
        Method::Get,
        "https://api.mojang.com/users/profiles/minecraft/Notch",
        200,
        r#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch"}"#,
    )
    .on(
        Method::Get,
        "https://sessionserver.mojang.com/session/minecraft/profile/069a79f444e94726a5befca90e38aaf5",
        200,
        format!(
            r#"{{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch",
                "properties": [{{"name": "textures", "value": "{textures}"}}]}}"#
        ),
    )
    .on(
        Method::Get,
        "https://api.mojang.com/users/profiles/minecraft/Nobody",
        404,
        "",
    );
    let player = Player::new(mock);
    let notch = player
        .lookup(&PlayerName::new("Notch").unwrap())
        .await
        .unwrap();
    assert_eq!(notch.model, SkinModel::Slim);
    assert_eq!(
        notch.skin.unwrap().as_str(),
        "http://textures.minecraft.net/texture/s"
    );
    assert!(notch.cape.is_some());
    assert!(matches!(
        player.lookup(&PlayerName::new("Nobody").unwrap()).await,
        Err(PlayerError::UnknownPlayer(_))
    ));
}
