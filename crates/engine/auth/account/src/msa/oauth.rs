use std::time::Duration;

use coblet_runtime::{HttpUrl, Secret};
use serde::Deserialize;

use super::MsaConfig;
use super::http::{Reply, encode_pairs, form_post, parse_url, send};
use crate::{AuthCtx, AuthError};

const AUTHORITY: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Extra seconds added to the polling interval on `slow_down`.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// Tokens from the Microsoft token endpoint.
pub(crate) struct MsTokens {
    pub(crate) access_token: Secret,
    pub(crate) refresh_token: Option<Secret>,
}

#[derive(Deserialize)]
struct RawTokens {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct RawError {
    error: String,
    #[serde(default)]
    error_description: String,
}

#[derive(Deserialize)]
struct RawDeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: Option<u64>,
    message: Option<String>,
}

/// Started device code login.
pub(crate) struct DeviceCode {
    pub(crate) code: Secret,
    pub(crate) user_code: String,
    pub(crate) verification_uri: HttpUrl,
    pub(crate) expires_in: Duration,
    pub(crate) interval: Duration,
    pub(crate) message: Option<String>,
}

/// Requests a device code.
pub(crate) async fn start_device_code(
    ctx: &AuthCtx,
    config: &MsaConfig,
) -> Result<DeviceCode, AuthError> {
    let request = form_post(
        &format!("{AUTHORITY}/devicecode"),
        &[
            ("client_id", config.client_id.as_str()),
            ("scope", &config.scopes),
        ],
    )?;
    let reply = send(ctx, "devicecode", request).await?;
    if !reply.is_success() {
        return Err(oauth_error(&reply));
    }
    let raw: RawDeviceCode = reply.json()?;
    let verification_uri =
        HttpUrl::new(raw.verification_uri).map_err(|error| AuthError::InvalidResponse {
            endpoint: "devicecode",
            message: error.to_string(),
        })?;
    Ok(DeviceCode {
        code: Secret::new(raw.device_code),
        user_code: raw.user_code,
        verification_uri,
        expires_in: Duration::from_secs(raw.expires_in),
        interval: Duration::from_secs(raw.interval.unwrap_or(5).max(1)),
        message: raw.message,
    })
}

/// Polls the token endpoint until the user has entered the code.
pub(crate) async fn poll_device_code(
    ctx: &AuthCtx,
    config: &MsaConfig,
    code: &DeviceCode,
) -> Result<MsTokens, AuthError> {
    let deadline = tokio::time::Instant::now() + code.expires_in;
    let mut interval = code.interval;
    loop {
        tokio::select! {
            () = ctx.cancel().cancelled() => return Err(AuthError::Cancelled),
            () = tokio::time::sleep(interval) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AuthError::Expired);
        }
        let request = form_post(
            &format!("{AUTHORITY}/token"),
            &[
                ("grant_type", DEVICE_CODE_GRANT),
                ("client_id", config.client_id.as_str()),
                ("device_code", code.code.expose()),
            ],
        )?;
        let reply = send(ctx, "token", request).await?;
        if reply.is_success() {
            return tokens(&reply);
        }
        match reply.json::<RawError>().map(|raw| raw.error).as_deref() {
            Ok("authorization_pending") => {}
            Ok("slow_down") => interval += SLOW_DOWN_STEP,
            _ => return Err(oauth_error(&reply)),
        }
    }
}

/// Returns the authorization page of a PKCE login.
pub(crate) fn authorize_url(
    config: &MsaConfig,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    login_hint: Option<&str>,
) -> Result<HttpUrl, AuthError> {
    let mut pairs = vec![
        ("client_id", config.client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("response_mode", "query"),
        ("scope", config.scopes.as_str()),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("prompt", "select_account"),
    ];
    if let Some(hint) = login_hint {
        pairs.push(("login_hint", hint));
    }
    parse_url(&format!("{AUTHORITY}/authorize?{}", encode_pairs(&pairs)))
}

/// Exchanges an authorization code for tokens.
pub(crate) async fn redeem_code(
    ctx: &AuthCtx,
    config: &MsaConfig,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<MsTokens, AuthError> {
    let request = form_post(
        &format!("{AUTHORITY}/token"),
        &[
            ("grant_type", "authorization_code"),
            ("client_id", config.client_id.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
            ("scope", &config.scopes),
        ],
    )?;
    let reply = send(ctx, "token", request).await?;
    if reply.is_success() {
        tokens(&reply)
    } else {
        Err(oauth_error(&reply))
    }
}

/// Exchanges a refresh token for new tokens; Microsoft rotates the refresh token.
pub(crate) async fn refresh(
    ctx: &AuthCtx,
    config: &MsaConfig,
    refresh_token: &Secret,
) -> Result<MsTokens, AuthError> {
    let request = form_post(
        &format!("{AUTHORITY}/token"),
        &[
            ("grant_type", "refresh_token"),
            ("client_id", config.client_id.as_str()),
            ("refresh_token", refresh_token.expose()),
            ("scope", &config.scopes),
        ],
    )?;
    let reply = send(ctx, "token", request).await?;
    if reply.is_success() {
        tokens(&reply)
    } else {
        Err(oauth_error(&reply))
    }
}

/// Maps an OAuth error from the redirect of a browser login.
pub(crate) fn redirect_error(error: &str, description: &str) -> AuthError {
    named_error(error, description)
}

fn tokens(reply: &Reply) -> Result<MsTokens, AuthError> {
    let raw: RawTokens = reply.json()?;
    Ok(MsTokens {
        access_token: Secret::new(raw.access_token),
        refresh_token: raw.refresh_token.map(Secret::new),
    })
}

fn oauth_error(reply: &Reply) -> AuthError {
    match reply.json::<RawError>() {
        Ok(raw) => named_error(&raw.error, &raw.error_description),
        Err(_) => reply.unexpected(),
    }
}

fn named_error(error: &str, description: &str) -> AuthError {
    match error {
        "invalid_grant" => AuthError::InvalidGrant,
        "authorization_declined" | "access_denied" => AuthError::Declined,
        "expired_token" | "code_expired" => AuthError::Expired,
        _ => AuthError::OAuth {
            error: error.to_owned(),
            description: description.lines().next().unwrap_or_default().to_owned(),
        },
    }
}
