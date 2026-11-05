use coblet_runtime::Secret;
use serde::Deserialize;
use serde_json::json;

use super::http::{Reply, json_post, send};
use crate::{AuthCtx, AuthError};

const USER_AUTH: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_AUTH: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MINECRAFT_RELYING_PARTY: &str = "rp://api.minecraftservices.com/";

/// Xbox Live token with its user hash.
pub(crate) struct XboxToken {
    pub(crate) token: Secret,
    pub(crate) user_hash: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawToken {
    token: String,
    display_claims: RawClaims,
}

#[derive(Deserialize)]
struct RawClaims {
    xui: Vec<RawUser>,
}

#[derive(Deserialize)]
struct RawUser {
    uhs: String,
}

#[derive(Deserialize)]
struct RawXboxError {
    #[serde(rename = "XErr")]
    code: u64,
}

/// Exchanges a Microsoft access token for an Xbox Live user token.
pub(crate) async fn user_token(ctx: &AuthCtx, ms_token: &Secret) -> Result<XboxToken, AuthError> {
    let body = json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={}", ms_token.expose()),
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT",
    });
    let request = json_post(USER_AUTH, &body)?.header("x-xbl-contract-version", "1");
    let reply = send(ctx, "xbox user", request).await?;
    parse(&reply)
}

/// Exchanges a user token for an XSTS token for Minecraft services.
pub(crate) async fn xsts_token(ctx: &AuthCtx, user: &XboxToken) -> Result<XboxToken, AuthError> {
    let body = json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [user.token.expose()],
        },
        "RelyingParty": MINECRAFT_RELYING_PARTY,
        "TokenType": "JWT",
    });
    let request = json_post(XSTS_AUTH, &body)?.header("x-xbl-contract-version", "1");
    let reply = send(ctx, "xsts", request).await?;
    parse(&reply)
}

fn parse(reply: &Reply) -> Result<XboxToken, AuthError> {
    if !reply.is_success() {
        return Err(match reply.json::<RawXboxError>() {
            Ok(raw) if reply.status == 401 || reply.status == 403 => xbox_error(raw.code),
            _ => reply.unexpected(),
        });
    }
    let raw: RawToken = reply.json()?;
    let user_hash = raw
        .display_claims
        .xui
        .into_iter()
        .next()
        .map(|user| user.uhs)
        .ok_or_else(|| AuthError::InvalidResponse {
            endpoint: reply.endpoint,
            message: "no user hash in the display claims".to_owned(),
        })?;
    Ok(XboxToken {
        token: Secret::new(raw.token),
        user_hash,
    })
}

/// Maps an `XErr` code to an error the player can act on.
pub(crate) fn xbox_error(code: u64) -> AuthError {
    match code {
        2_148_916_227 => AuthError::XboxBanned,
        2_148_916_233 => AuthError::NoXboxAccount,
        2_148_916_235 => AuthError::RegionUnavailable,
        2_148_916_236 | 2_148_916_237 => AuthError::AdultVerificationRequired,
        2_148_916_238 => AuthError::ChildAccount,
        other => AuthError::Xbox(other),
    }
}
