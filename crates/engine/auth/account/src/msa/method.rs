use std::time::Duration;

use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
use coblet_runtime::{AuthPrompt, BoxFuture, Extension, Secret};
use coblet_types::ExtensionId;

use super::http::query_pairs;
use super::loopback::Loopback;
use super::{MsaConfig, minecraft, oauth, random, xbox};
use crate::{
    Account, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError, AuthFlow, AuthMethod,
    CallbackReceiver, LaunchSession, LicenseCheck, LoginOptions, PlayerProfile, Redirect,
    RefreshMode, Refreshed, callback_channel,
};

/// ID of the built-in Microsoft method.
pub const MICROSOFT_METHOD: &str = "coblet:microsoft";

const REFRESH_TOKEN: &str = "refresh_token";
const MINECRAFT_TOKEN: &str = "minecraft_token";
const XUID: &str = "xuid";

/// A cached session this close to expiry is renewed before a launch.
const EXPIRY_MARGIN: Duration = Duration::from_mins(5);

/// How long a browser login waits for its redirect.
const BROWSER_TIMEOUT: Duration = Duration::from_mins(15);

/// Microsoft accounts: OAuth with the device code or PKCE flow, then Xbox
/// Live, XSTS and Minecraft services.
///
/// The account keeps the Microsoft refresh token and the Minecraft access
/// token as secrets. [`refresh`](AuthMethod::refresh) reuses the Minecraft
/// token until five minutes before it expires; renewing it rotates the
/// refresh token, which is saved before the remaining steps run.
#[derive(Debug)]
pub struct MicrosoftMethod {
    id: ExtensionId,
    config: MsaConfig,
}

impl MicrosoftMethod {
    /// Creates the method with the launcher's Client ID.
    #[must_use]
    pub fn new(config: MsaConfig) -> Self {
        Self {
            id: ExtensionId::new(MICROSOFT_METHOD).unwrap_or_else(|_| unreachable!()),
            config,
        }
    }

    /// Returns the settings.
    #[must_use]
    pub fn config(&self) -> &MsaConfig {
        &self.config
    }

    async fn device_code(&self, ctx: &AuthCtx) -> Result<AuthFlow, AuthError> {
        let code = oauth::start_device_code(ctx, &self.config).await?;
        let prompt = AuthPrompt::DeviceCode {
            user_code: code.user_code.clone(),
            verification_uri: code.verification_uri.clone(),
            expires_at: ctx.now() + code.expires_in,
            message: code.message.clone(),
        };
        ctx.publish(&prompt);
        let ctx = ctx.clone();
        let config = self.config.clone();
        Ok(AuthFlow::new(prompt, async move {
            let tokens = oauth::poll_device_code(&ctx, &config, &code).await?;
            complete_login(&ctx, tokens).await
        }))
    }

    async fn browser(
        &self,
        ctx: &AuthCtx,
        options: &LoginOptions,
        redirect: &Redirect,
    ) -> Result<AuthFlow, AuthError> {
        let pkce = random::Pkce::generate()?;
        let state = random::token(16)?;
        let (redirect_uri, listener) = match redirect {
            Redirect::Loopback { host, port, path } => {
                let listener = Loopback::bind(host, *port, path).await?;
                (listener.redirect_uri().to_owned(), Some(listener))
            }
            Redirect::External { redirect_uri } => (redirect_uri.clone(), None),
        };
        let url = oauth::authorize_url(
            &self.config,
            &redirect_uri,
            &state,
            &pkce.challenge,
            options.login_hint.as_deref(),
        )?;
        let prompt = AuthPrompt::OpenUrl {
            url,
            redirect_uri: redirect_uri.clone(),
        };
        ctx.publish(&prompt);
        let (sender, receiver) = callback_channel();
        let ctx = ctx.clone();
        let config = self.config.clone();
        let flow = AuthFlow::new(prompt, async move {
            let uri = wait_for_redirect(&ctx, receiver, listener).await?;
            let code = authorization_code(&uri, &state)?;
            let tokens =
                oauth::redeem_code(&ctx, &config, &code, &redirect_uri, &pkce.verifier).await?;
            complete_login(&ctx, tokens).await
        });
        Ok(flow.with_callback(sender))
    }

    async fn renew(&self, ctx: &AuthCtx, secrets: &AccountSecrets) -> Result<Refreshed, AuthError> {
        let refresh_token = secrets
            .get(REFRESH_TOKEN)
            .await?
            .ok_or(AuthError::InvalidGrant)?;
        let tokens = oauth::refresh(ctx, &self.config, &refresh_token).await?;
        if let Some(rotated) = tokens.refresh_token {
            secrets.put(REFRESH_TOKEN, rotated).await?;
        }
        let (update, minecraft_token) = minecraft_account(ctx, &tokens.access_token).await?;
        secrets
            .put(MINECRAFT_TOKEN, minecraft_token.clone())
            .await?;
        let session = session_for(&update, &minecraft_token);
        Ok(Refreshed::new(update, session))
    }
}

impl Extension for MicrosoftMethod {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &'static str {
        "Microsoft"
    }
}

impl AuthMethod for MicrosoftMethod {
    fn begin<'a>(
        &'a self,
        ctx: &'a AuthCtx,
        options: &'a LoginOptions,
    ) -> BoxFuture<'a, Result<AuthFlow, AuthError>> {
        Box::pin(async move {
            match &options.interaction {
                crate::Interaction::DeviceCode => self.device_code(ctx).await,
                crate::Interaction::Browser(redirect) => self.browser(ctx, options, redirect).await,
            }
        })
    }

    fn refresh<'a>(
        &'a self,
        ctx: &'a AuthCtx,
        account: &'a Account,
        secrets: &'a AccountSecrets,
        mode: RefreshMode,
    ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
        Box::pin(async move {
            let unexpired = account
                .session_expires_at
                .is_some_and(|expires| expires > ctx.now() + EXPIRY_MARGIN);
            // A session also works for reading the license when the account
            // could not play at its last check.
            let usable = match mode {
                RefreshMode::IfNeeded => account.state == AccountState::Ready,
                RefreshMode::Recheck => matches!(
                    account.state,
                    AccountState::Ready | AccountState::NoLicense | AccountState::NoProfile
                ),
                RefreshMode::Force => false,
            };
            if unexpired
                && usable
                && let Some(token) = secrets.get(MINECRAFT_TOKEN).await?
            {
                if mode == RefreshMode::Recheck {
                    match recheck(ctx, account, &token).await {
                        Ok(update) => {
                            let session = session_for(&update, &token);
                            return Ok(Refreshed::new(update, session));
                        }
                        // The token was revoked early; renew it below.
                        Err(AuthError::Http { status: 401, .. }) => {}
                        Err(error) => return Err(error),
                    }
                } else {
                    let update = AccountUpdate::from_account(account);
                    if let Some(session) = session_for(&update, &token) {
                        return Ok(Refreshed::new(update, Some(session)));
                    }
                }
            }
            self.renew(ctx, secrets).await
        })
    }
}

/// Runs the Xbox and Minecraft steps of a new login and adds its secrets.
async fn complete_login(
    ctx: &AuthCtx,
    tokens: oauth::MsTokens,
) -> Result<AccountUpdate, AuthError> {
    let refresh_token = tokens
        .refresh_token
        .ok_or_else(|| AuthError::InvalidResponse {
            endpoint: "token",
            message: "no refresh token was issued, the scopes need offline_access".to_owned(),
        })?;
    let (mut update, minecraft_token) = minecraft_account(ctx, &tokens.access_token).await?;
    update.secrets.insert(REFRESH_TOKEN, refresh_token);
    update.secrets.insert(MINECRAFT_TOKEN, minecraft_token);
    Ok(update)
}

/// Turns a Microsoft access token into account data and a Minecraft token.
async fn minecraft_account(
    ctx: &AuthCtx,
    ms_token: &Secret,
) -> Result<(AccountUpdate, Secret), AuthError> {
    let user = xbox::user_token(ctx, ms_token).await?;
    let xsts = xbox::xsts_token(ctx, &user).await?;
    let minecraft = minecraft::login(ctx, &xsts).await?;
    let mut update = AccountUpdate::new(AccountState::Ready);
    read_entitlements(ctx, &minecraft.access_token, &mut update).await?;
    update.session_expires_at = Some(ctx.now() + minecraft.expires_in);
    if let Some(xuid) = minecraft::xuid(&minecraft.access_token) {
        update.data.insert(XUID.to_owned(), xuid);
    }
    Ok((update, minecraft.access_token))
}

/// Reads the license and profile of an account whose session is still
/// valid, keeping its expiry and data.
async fn recheck(
    ctx: &AuthCtx,
    account: &Account,
    token: &Secret,
) -> Result<AccountUpdate, AuthError> {
    let mut update = AccountUpdate::from_account(account);
    read_entitlements(ctx, token, &mut update).await?;
    Ok(update)
}

/// Sets the license, profile and state of `update` from the services.
async fn read_entitlements(
    ctx: &AuthCtx,
    token: &Secret,
    update: &mut AccountUpdate,
) -> Result<(), AuthError> {
    let license = minecraft::license(ctx, token).await?;
    let profile = minecraft::profile(ctx, token).await?;
    update.state = match (license.may_play(), &profile) {
        (true, Some(_)) => AccountState::Ready,
        (true, None) => AccountState::NoProfile,
        (false, _) => AccountState::NoLicense,
    };
    update.license = Some(LicenseCheck::new(license, ctx.now()));
    update.profile = profile;
    Ok(())
}

/// Builds the launch session of a ready account.
fn session_for(update: &AccountUpdate, token: &Secret) -> Option<LaunchSession> {
    if update.state != AccountState::Ready {
        return None;
    }
    let profile: &PlayerProfile = update.profile.as_ref()?;
    let mut online = OnlineSession::new(
        profile.name.clone(),
        profile.uuid.clone(),
        Credential::new(token.expose()),
    );
    online.xuid = update.data.get(XUID).map(Credential::new);
    let session = LaunchSession::new(LaunchAuth::online(online));
    Some(match update.session_expires_at {
        Some(expires) => session.with_expiry(expires),
        None => session,
    })
}

/// Checks the redirect of a browser login and returns its code.
fn authorization_code(uri: &str, state: &str) -> Result<String, AuthError> {
    let pairs = query_pairs(uri)
        .ok_or_else(|| AuthError::InvalidCallback("the query is not valid utf-8".to_owned()))?;
    let get = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if get("state") != Some(state) {
        return Err(AuthError::StateMismatch);
    }
    if let Some(error) = get("error") {
        return Err(oauth::redirect_error(
            error,
            get("error_description").unwrap_or_default(),
        ));
    }
    get("code")
        .map(str::to_owned)
        .ok_or_else(|| AuthError::InvalidCallback("the redirect carries no code".to_owned()))
}

async fn next_redirect(listener: Option<&Loopback>) -> String {
    match listener {
        Some(listener) => listener.next_redirect().await,
        None => std::future::pending().await,
    }
}

/// Waits for the redirect from the listener or the caller.
async fn wait_for_redirect(
    ctx: &AuthCtx,
    receiver: CallbackReceiver,
    listener: Option<Loopback>,
) -> Result<String, AuthError> {
    tokio::select! {
        () = ctx.cancel().cancelled() => Err(AuthError::Cancelled),
        () = tokio::time::sleep(BROWSER_TIMEOUT) => Err(AuthError::Expired),
        uri = next_redirect(listener.as_ref()) => Ok(uri),
        uri = receiver.recv() => uri,
    }
}

#[cfg(test)]
#[path = "method_tests.rs"]
mod tests;
