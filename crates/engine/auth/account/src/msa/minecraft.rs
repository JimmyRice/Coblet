use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coblet_runtime::Secret;
use serde::Deserialize;
use serde_json::json;

use super::http::{bearer_get, json_post, send};
use super::random;
use super::xbox::XboxToken;
use crate::{AuthCtx, AuthError, GameLicense, PlayerProfile};

const SERVICES: &str = "https://api.minecraftservices.com";

/// Minecraft access token.
pub(crate) struct MinecraftToken {
    pub(crate) access_token: Secret,
    pub(crate) expires_in: Duration,
}

#[derive(Deserialize)]
struct RawLogin {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct RawEntitlements {
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Deserialize)]
struct RawItem {
    name: String,
}

/// Signs in to Minecraft services with an XSTS token.
pub(crate) async fn login(ctx: &AuthCtx, xsts: &XboxToken) -> Result<MinecraftToken, AuthError> {
    let identity = format!("XBL3.0 x={};{}", xsts.user_hash, xsts.token.expose());
    let request = json_post(
        &format!("{SERVICES}/authentication/login_with_xbox"),
        &json!({ "identityToken": identity }),
    )?;
    let reply = send(ctx, "login_with_xbox", request).await?;
    if reply.status == 403 && reply.message().contains("Invalid app registration") {
        return Err(AuthError::ClientNotApproved);
    }
    if !reply.is_success() {
        return Err(reply.unexpected());
    }
    let raw: RawLogin = reply.json()?;
    Ok(MinecraftToken {
        access_token: Secret::new(raw.access_token),
        expires_in: Duration::from_secs(raw.expires_in),
    })
}

/// Reads what the account's entitlements say about the game.
pub(crate) async fn license(ctx: &AuthCtx, token: &Secret) -> Result<GameLicense, AuthError> {
    let url = format!(
        "{SERVICES}/entitlements/license?requestId={}",
        random::uuid()?
    );
    let request = bearer_get(&url, token.expose())?;
    let reply = send(ctx, "entitlements", request).await?;
    if !reply.is_success() {
        return Err(reply.unexpected());
    }
    let raw: RawEntitlements = reply.json()?;
    Ok(GameLicense::from_entitlements(
        raw.items.iter().map(|item| item.name.as_str()),
    ))
}

/// Fetches the player profile; `None` when the account has not created one.
pub(crate) async fn profile(
    ctx: &AuthCtx,
    token: &Secret,
) -> Result<Option<PlayerProfile>, AuthError> {
    let request = bearer_get(&format!("{SERVICES}/minecraft/profile"), token.expose())?;
    let reply = send(ctx, "profile", request).await?;
    if reply.status == 404 {
        return Ok(None);
    }
    if !reply.is_success() {
        return Err(reply.unexpected());
    }
    PlayerProfile::from_services_json(&reply.body)
        .map(Some)
        .map_err(|error| AuthError::InvalidResponse {
            endpoint: "profile",
            message: error.to_string(),
        })
}

/// Reads the `xuid` claim of a Minecraft access token, if it has one.
pub(crate) fn xuid(token: &Secret) -> Option<String> {
    let payload = token.expose().split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    match claims.get("xuid")? {
        serde_json::Value::String(xuid) => Some(xuid.clone()),
        serde_json::Value::Number(xuid) => Some(xuid.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use coblet_runtime::Secret;

    use super::xuid;

    #[test]
    fn reads_the_xuid_claim() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"xuid":"2535400000000000","sub":"x"}"#);
        let token = Secret::new(format!("eyJhbGciOiJIUzI1NiJ9.{payload}.sig"));
        assert_eq!(xuid(&token).as_deref(), Some("2535400000000000"));
        assert_eq!(xuid(&Secret::new("opaque")), None);
    }
}
