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
    fn appearance_service(&self) -> Option<ExtensionId> {
        ExtensionId::new(crate::MINECRAFT_SERVICES).ok()
    }

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
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use coblet_launch_command::LaunchAuth;
    use coblet_runtime::{
        AuthPrompt, Event, EventBus, FixedClock, MemorySecretStore, Method, Received, Secret,
    };

    use super::super::mock::{
        DEVICE_CODE, ENTITLEMENTS, LOGIN, MockServices, PROFILE, TOKEN, XSTS, form_value,
        minecraft_token,
    };
    use super::super::random::challenge;
    use super::super::{MsaClientId, MsaConfig};
    use super::{MICROSOFT_METHOD, MicrosoftMethod};
    use crate::{
        Account, AccountId, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError,
        AuthMethod, GameLicense, Interaction, LicenseCheck, LoginOptions, Redirect, RefreshMode,
    };

    const NOW: Duration = Duration::from_hours(500_000);

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + NOW
    }

    #[test]
    fn accounts_use_minecraft_services_for_skins_and_capes() {
        assert_eq!(
            method().appearance_service().unwrap().as_str(),
            crate::MINECRAFT_SERVICES
        );
        assert!(crate::OfflineMethod::new().appearance_service().is_none());
    }

    fn method() -> MicrosoftMethod {
        MicrosoftMethod::new(MsaConfig::new(
            MsaClientId::new("00000000-1111-2222-3333-444444444444").unwrap(),
        ))
    }

    fn ctx(mock: &Arc<MockServices>) -> AuthCtx {
        AuthCtx::new(mock.clone()).with_clock(Arc::new(FixedClock(now())))
    }

    fn device_code(mock: &MockServices, interval: u64, expires_in: u64) {
        mock.on(
            Method::Post,
            DEVICE_CODE,
            200,
            &format!(
                r#"{{"device_code": "dc-1", "user_code": "ABCD-1234",
                "verification_uri": "https://www.microsoft.com/link",
                "expires_in": {expires_in}, "interval": {interval}, "message": "Enter the code"}}"#
            ),
        );
    }

    const MS_TOKENS: &str =
        r#"{"access_token": "ms-access", "refresh_token": "ms-refresh", "expires_in": 3600}"#;

    async fn device_login(mock: &Arc<MockServices>) -> Result<AccountUpdate, AuthError> {
        let ctx = ctx(mock);
        let flow = method().begin(&ctx, &LoginOptions::new()).await?;
        flow.finish().await
    }

    fn ready_mock() -> Arc<MockServices> {
        let mock = MockServices::new();
        device_code(&mock, 1, 900);
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS);
        mock.xbox_and_minecraft();
        mock
    }

    #[tokio::test(start_paused = true)]
    async fn device_code_login_creates_a_ready_account() {
        let mock = MockServices::new();
        device_code(&mock, 5, 900);
        mock.on(
            Method::Post,
            TOKEN,
            400,
            r#"{"error": "authorization_pending"}"#,
        )
        .on(Method::Post, TOKEN, 400, r#"{"error": "slow_down"}"#)
        .on(Method::Post, TOKEN, 200, MS_TOKENS)
        .xbox_and_minecraft();
        let events = EventBus::new(8);
        let mut subscription = events.subscribe();
        let ctx = ctx(&mock).with_events(events);
        let started = tokio::time::Instant::now();

        let flow = method().begin(&ctx, &LoginOptions::new()).await.unwrap();
        let AuthPrompt::DeviceCode {
            user_code,
            verification_uri,
            expires_at,
            message,
        } = flow.prompt().clone()
        else {
            panic!("expected a device code prompt, got {:?}", flow.prompt());
        };
        assert_eq!(user_code, "ABCD-1234");
        assert_eq!(verification_uri.as_str(), "https://www.microsoft.com/link");
        assert_eq!(expires_at, now() + Duration::from_mins(15));
        assert_eq!(message.as_deref(), Some("Enter the code"));
        assert!(matches!(
            subscription.recv().await,
            Some(Received::Event(Event::AuthPrompt(
                AuthPrompt::DeviceCode { .. }
            )))
        ));

        let update = flow.finish().await.unwrap();
        assert_eq!(update.state, AccountState::Ready);
        let profile = update.profile.unwrap();
        assert_eq!(profile.name.as_str(), "Notch");
        assert_eq!(profile.uuid, "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(
            update.session_expires_at,
            Some(now() + Duration::from_hours(24))
        );
        assert_eq!(
            update.data.get("xuid").map(String::as_str),
            Some("2535400000000001")
        );
        assert_eq!(
            update.secrets.get("refresh_token").map(Secret::expose),
            Some("ms-refresh")
        );
        assert_eq!(
            update.secrets.get("minecraft_token").map(Secret::expose),
            Some(minecraft_token().as_str())
        );

        // Polled after 5 s, then 5 s, then 10 s once asked to slow down.
        assert_eq!(started.elapsed(), Duration::from_secs(20));
        let polls = mock.seen_at(TOKEN);
        assert_eq!(polls.len(), 3);
        assert_eq!(
            form_value(&polls[0].body, "grant_type").as_deref(),
            Some("urn:ietf:params:oauth:grant-type:device_code")
        );
        assert_eq!(
            form_value(&polls[0].body, "device_code").as_deref(),
            Some("dc-1")
        );
        let start = &mock.seen_at(DEVICE_CODE)[0];
        assert_eq!(
            form_value(&start.body, "scope").as_deref(),
            Some("XboxLive.signin offline_access")
        );
        assert!(mock.seen_at(XSTS)[0].body.contains("xbl-token"));
        assert!(
            mock.seen_at(LOGIN)[0]
                .body
                .contains("XBL3.0 x=uhs1;xsts-token")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn device_code_failures_have_their_own_errors() {
        for (reply, expected) in [
            (r#"{"error": "authorization_declined"}"#, "declined"),
            (r#"{"error": "expired_token"}"#, "expired"),
            (
                r#"{"error": "invalid_client", "error_description": "AADSTS7000218: bad client\r\nTrace"}"#,
                "oauth",
            ),
        ] {
            let mock = MockServices::new();
            device_code(&mock, 1, 900);
            mock.on(Method::Post, TOKEN, 400, reply);
            let error = device_login(&mock).await.unwrap_err();
            let matched = match expected {
                "declined" => matches!(error, AuthError::Declined),
                "expired" => matches!(error, AuthError::Expired),
                _ => matches!(
                    &error,
                    AuthError::OAuth { error, description }
                        if error == "invalid_client" && description == "AADSTS7000218: bad client"
                ),
            };
            assert!(matched, "{reply} gave {error:?}");
        }

        let mock = MockServices::new();
        device_code(&mock, 5, 3);
        assert!(matches!(device_login(&mock).await, Err(AuthError::Expired)));
        assert!(mock.seen_at(TOKEN).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_stops_the_polling() {
        let mock = MockServices::new();
        device_code(&mock, 5, 900);
        mock.on(
            Method::Post,
            TOKEN,
            400,
            r#"{"error": "authorization_pending"}"#,
        );
        let ctx = ctx(&mock);
        let flow = method().begin(&ctx, &LoginOptions::new()).await.unwrap();
        let cancel = ctx.cancel().clone();
        let finished = tokio::spawn(flow.finish());
        tokio::time::sleep(Duration::from_secs(12)).await;
        cancel.cancel();
        assert!(matches!(finished.await.unwrap(), Err(AuthError::Cancelled)));
        assert_eq!(mock.seen_at(TOKEN).len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn xbox_refusals_tell_the_player_what_to_do() {
        for (code, expected) in [
            (2_148_916_233_u64, AuthError::NoXboxAccount),
            (2_148_916_235, AuthError::RegionUnavailable),
            (2_148_916_236, AuthError::AdultVerificationRequired),
            (2_148_916_238, AuthError::ChildAccount),
            (2_148_916_227, AuthError::XboxBanned),
            (2_148_916_299, AuthError::Xbox(2_148_916_299)),
        ] {
            let mock = MockServices::new();
            device_code(&mock, 1, 900);
            mock.on(Method::Post, TOKEN, 200, MS_TOKENS).on(
                Method::Post,
                XSTS,
                401,
                &format!(r#"{{"Identity": "0", "XErr": {code}, "Message": "", "Redirect": ""}}"#),
            );
            mock.xbox_and_minecraft();
            let error = device_login(&mock).await.unwrap_err();
            assert_eq!(error.to_string(), expected.to_string());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn recognizes_an_unapproved_client_id() {
        let mock = MockServices::new();
        device_code(&mock, 1, 900);
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS).on(
        Method::Post,
        LOGIN,
        403,
        r#"{"path": "/authentication/login_with_xbox", "errorMessage": "Invalid app registration, see https://aka.ms/AppRegInfo for more information"}"#,
    );
        mock.xbox_and_minecraft();
        assert!(matches!(
            device_login(&mock).await,
            Err(AuthError::ClientNotApproved)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn login_records_how_the_account_holds_the_game() {
        let mock = ready_mock();
        let update = device_login(&mock).await.unwrap();
        assert_eq!(
            update.license,
            Some(LicenseCheck::new(GameLicense::Purchased, now()))
        );
        let url = &mock.seen_at(ENTITLEMENTS)[0].url;
        let request_id = url
            .strip_prefix(&format!("{ENTITLEMENTS}?requestId="))
            .unwrap();
        assert_eq!(request_id.len(), 36, "{url}");

        let mock = MockServices::new();
        device_code(&mock, 1, 900);
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS).on(
            Method::Get,
            ENTITLEMENTS,
            200,
            r#"{"items": [{"name": "game_minecraft"}]}"#,
        );
        mock.xbox_and_minecraft();
        let update = device_login(&mock).await.unwrap();
        assert_eq!(update.state, AccountState::Ready);
        assert_eq!(update.license.unwrap().license, GameLicense::Entitled);
    }

    #[tokio::test(start_paused = true)]
    async fn marks_accounts_without_license_or_profile() {
        let mock = MockServices::new();
        device_code(&mock, 1, 900);
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS)
            .on(Method::Get, ENTITLEMENTS, 200, r#"{"items": []}"#)
            .on(Method::Get, PROFILE, 404, r#"{"error": "NOT_FOUND"}"#);
        mock.xbox_and_minecraft();
        let update = device_login(&mock).await.unwrap();
        assert_eq!(update.state, AccountState::NoLicense);
        assert_eq!(
            update.license,
            Some(LicenseCheck::new(GameLicense::Missing, now()))
        );
        assert!(update.profile.is_none());
        assert!(update.secrets.get("refresh_token").is_some());

        let mock = MockServices::new();
        device_code(&mock, 1, 900);
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS).on(
            Method::Get,
            PROFILE,
            404,
            r#"{"error": "NOT_FOUND"}"#,
        );
        mock.xbox_and_minecraft();
        let update = device_login(&mock).await.unwrap();
        assert_eq!(update.state, AccountState::NoProfile);
    }

    fn prompt_param(prompt: &AuthPrompt, name: &str) -> String {
        let AuthPrompt::OpenUrl { url, .. } = prompt else {
            panic!("expected a browser prompt, got {prompt:?}");
        };
        super::super::http::query_pairs(url.as_str())
            .unwrap()
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
            .unwrap()
    }

    fn external() -> LoginOptions {
        LoginOptions::new()
            .with_hint("player@example.com")
            .with_interaction(Interaction::Browser(Redirect::External {
                redirect_uri: "myapp://auth".to_owned(),
            }))
    }

    #[tokio::test]
    async fn browser_login_checks_state_and_sends_the_verifier() {
        let mock = MockServices::new();
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS);
        mock.xbox_and_minecraft();
        let ctx = ctx(&mock);
        let flow = method().begin(&ctx, &external()).await.unwrap();
        let prompt = flow.prompt().clone();
        assert!(matches!(
            &prompt,
            AuthPrompt::OpenUrl { url, redirect_uri }
                if redirect_uri == "myapp://auth"
                    && url.as_str().starts_with("https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?")
        ));
        assert_eq!(prompt_param(&prompt, "login_hint"), "player@example.com");
        assert_eq!(prompt_param(&prompt, "code_challenge_method"), "S256");
        let state = prompt_param(&prompt, "state");

        flow.callback()
            .unwrap()
            .submit(format!("myapp://auth?code=C-1&state={state}"))
            .unwrap();
        let update = flow.finish().await.unwrap();
        assert_eq!(update.state, AccountState::Ready);

        let redeem = &mock.seen_at(TOKEN)[0];
        assert_eq!(
            form_value(&redeem.body, "grant_type").as_deref(),
            Some("authorization_code")
        );
        assert_eq!(form_value(&redeem.body, "code").as_deref(), Some("C-1"));
        assert_eq!(
            form_value(&redeem.body, "redirect_uri").as_deref(),
            Some("myapp://auth")
        );
        let verifier = form_value(&redeem.body, "code_verifier").unwrap();
        assert_eq!(
            challenge(&verifier),
            prompt_param(&prompt, "code_challenge")
        );
    }

    #[tokio::test]
    async fn browser_login_rejects_foreign_or_failed_redirects() {
        let mock = MockServices::new();
        let ctx = ctx(&mock);
        let flow = method().begin(&ctx, &external()).await.unwrap();
        flow.callback()
            .unwrap()
            .submit("myapp://auth?code=C-1&state=other")
            .unwrap();
        assert!(matches!(flow.finish().await, Err(AuthError::StateMismatch)));

        let flow = method().begin(&ctx, &external()).await.unwrap();
        let state = prompt_param(flow.prompt(), "state");
        flow.callback()
            .unwrap()
            .submit(format!(
                "myapp://auth?error=access_denied&error_description=user+said+no&state={state}"
            ))
            .unwrap();
        assert!(matches!(flow.finish().await, Err(AuthError::Declined)));
        assert!(mock.seen().is_empty());
    }

    fn account(state: AccountState, expires_in: Duration) -> Account {
        let mut account = Account::new(
            AccountId::new("acc").unwrap(),
            coblet_types::ExtensionId::new(MICROSOFT_METHOD).unwrap(),
        );
        account.profile = Some(
            crate::PlayerProfile::from_services_json(super::super::mock::PROFILE_JSON.as_bytes())
                .unwrap(),
        );
        account.state = state;
        account.session_expires_at = Some(now() + expires_in);
        account.data.insert("xuid".to_owned(), "42".to_owned());
        account
    }

    async fn secrets_for(account: &Account, refresh: Option<&str>) -> AccountSecrets {
        let secrets = AccountSecrets::new(
            Arc::new(MemorySecretStore::new()),
            &account.method,
            &account.id,
        );
        secrets
            .put("minecraft_token", Secret::new("cached-mc"))
            .await
            .unwrap();
        if let Some(refresh) = refresh {
            secrets
                .put("refresh_token", Secret::new(refresh))
                .await
                .unwrap();
        }
        secrets
    }

    #[tokio::test]
    async fn refresh_reuses_a_fresh_session_without_network() {
        let mock = MockServices::new();
        let account = account(AccountState::Ready, Duration::from_secs(3600));
        let secrets = secrets_for(&account, Some("old-refresh")).await;
        let refreshed = method()
            .refresh(&ctx(&mock), &account, &secrets, RefreshMode::IfNeeded)
            .await
            .unwrap();
        let LaunchAuth::Online(session) = refreshed.session.unwrap().auth else {
            panic!("expected an online session");
        };
        assert_eq!(session.access_token.expose(), "cached-mc");
        assert_eq!(session.uuid, "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(session.xuid.unwrap().expose(), "42");
        assert!(mock.seen().is_empty());
    }

    #[tokio::test]
    async fn refresh_renews_expiring_sessions_and_saves_the_rotated_token() {
        let mock = MockServices::new();
        mock.on(
            Method::Post,
            TOKEN,
            200,
            r#"{"access_token": "ms-access-2", "refresh_token": "rotated", "expires_in": 3600}"#,
        );
        mock.xbox_and_minecraft();
        let account = account(AccountState::Ready, Duration::from_secs(60));
        let secrets = secrets_for(&account, Some("old-refresh")).await;
        let refreshed = method()
            .refresh(&ctx(&mock), &account, &secrets, RefreshMode::IfNeeded)
            .await
            .unwrap();
        assert_eq!(
            form_value(&mock.seen_at(TOKEN)[0].body, "refresh_token").as_deref(),
            Some("old-refresh")
        );
        assert_eq!(
            secrets
                .get("refresh_token")
                .await
                .unwrap()
                .unwrap()
                .expose(),
            "rotated"
        );
        assert_eq!(
            secrets
                .get("minecraft_token")
                .await
                .unwrap()
                .unwrap()
                .expose(),
            minecraft_token()
        );
        let session = refreshed.session.unwrap();
        assert_eq!(session.access_token().unwrap().expose(), minecraft_token());
        assert_eq!(session.expires_at, Some(now() + Duration::from_hours(24)));
        assert_eq!(refreshed.update.state, AccountState::Ready);
    }

    #[tokio::test]
    async fn refresh_keeps_the_rotated_token_when_a_later_step_fails() {
        let mock = MockServices::new();
        mock.on(
            Method::Post,
            TOKEN,
            200,
            r#"{"access_token": "ms-access-2", "refresh_token": "rotated"}"#,
        )
        .on(Method::Post, XSTS, 503, "busy");
        mock.xbox_and_minecraft();
        let account = account(AccountState::Ready, Duration::ZERO);
        let secrets = secrets_for(&account, Some("old-refresh")).await;
        let error = method()
            .refresh(&ctx(&mock), &account, &secrets, RefreshMode::Force)
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Http { status: 503, .. }));
        assert!(error.is_retryable());
        assert_eq!(
            secrets
                .get("refresh_token")
                .await
                .unwrap()
                .unwrap()
                .expose(),
            "rotated"
        );
    }

    #[tokio::test]
    async fn refresh_asks_for_a_new_login_when_the_token_is_gone() {
        let mock = MockServices::new();
        mock.on(
            Method::Post,
            TOKEN,
            400,
            r#"{"error": "invalid_grant", "error_description": "AADSTS70000: revoked"}"#,
        );
        let account = account(AccountState::Ready, Duration::ZERO);
        let secrets = secrets_for(&account, Some("old-refresh")).await;
        let error = method()
            .refresh(&ctx(&mock), &account, &secrets, RefreshMode::Force)
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::InvalidGrant));
        assert_eq!(error.implied_state(), Some(AccountState::NeedsReauth));

        let secrets = secrets_for(&account, None).await;
        assert!(matches!(
            method()
                .refresh(&ctx(&mock), &account, &secrets, RefreshMode::IfNeeded)
                .await,
            Err(AuthError::InvalidGrant)
        ));
    }

    #[tokio::test]
    async fn refresh_rechecks_accounts_that_could_not_play() {
        let mock = ready_mock();
        let account = account(AccountState::NoLicense, Duration::from_secs(3600));
        let secrets = secrets_for(&account, Some("old-refresh")).await;
        let refreshed = method()
            .refresh(&ctx(&mock), &account, &secrets, RefreshMode::IfNeeded)
            .await
            .unwrap();
        assert_eq!(refreshed.update.state, AccountState::Ready);
        assert!(refreshed.session.is_some());
    }

    #[tokio::test]
    async fn recheck_reads_the_license_with_the_current_session() {
        let mock = MockServices::new();
        mock.xbox_and_minecraft();
        let account = account(AccountState::NoLicense, Duration::from_secs(3600));
        let secrets = secrets_for(&account, Some("old-refresh")).await;
        let refreshed = method()
            .refresh(&ctx(&mock), &account, &secrets, RefreshMode::Recheck)
            .await
            .unwrap();
        let urls: Vec<_> = mock.seen().into_iter().map(|seen| seen.url).collect();
        assert_eq!(urls.len(), 2, "{urls:?}");
        assert!(
            urls[0].starts_with(ENTITLEMENTS) && urls[1] == PROFILE,
            "{urls:?}"
        );
        let update = &refreshed.update;
        assert_eq!(update.state, AccountState::Ready);
        assert_eq!(
            update.license,
            Some(LicenseCheck::new(GameLicense::Purchased, now()))
        );
        assert_eq!(update.session_expires_at, account.session_expires_at);
        assert_eq!(update.data, account.data);
        let session = refreshed.session.unwrap();
        assert_eq!(session.access_token().unwrap().expose(), "cached-mc");
    }

    #[tokio::test]
    async fn recheck_notices_a_lost_license() {
        let mock = MockServices::new();
        mock.on(Method::Get, ENTITLEMENTS, 200, r#"{"items": []}"#)
            .on(Method::Get, PROFILE, 200, super::super::mock::PROFILE_JSON);
        let account = account(AccountState::Ready, Duration::from_secs(3600));
        let secrets = secrets_for(&account, Some("old-refresh")).await;
        let refreshed = method()
            .refresh(&ctx(&mock), &account, &secrets, RefreshMode::Recheck)
            .await
            .unwrap();
        assert_eq!(refreshed.update.state, AccountState::NoLicense);
        assert_eq!(
            refreshed.update.license.unwrap().license,
            GameLicense::Missing
        );
        assert!(refreshed.session.is_none());
    }

    #[tokio::test]
    async fn recheck_renews_a_rejected_or_expiring_session() {
        let mock = MockServices::new();
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS)
            .on(Method::Get, ENTITLEMENTS, 401, "");
        mock.xbox_and_minecraft();
        let rejected = account(AccountState::Ready, Duration::from_secs(3600));
        let secrets = secrets_for(&rejected, Some("old-refresh")).await;
        let refreshed = method()
            .refresh(&ctx(&mock), &rejected, &secrets, RefreshMode::Recheck)
            .await
            .unwrap();
        assert_eq!(mock.seen_at(TOKEN).len(), 1);
        assert_eq!(mock.seen_at(ENTITLEMENTS).len(), 2);
        assert_eq!(
            refreshed.update.license.unwrap().license,
            GameLicense::Purchased
        );
        let session = refreshed.session.unwrap();
        assert_eq!(session.access_token().unwrap().expose(), minecraft_token());

        let mock = MockServices::new();
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS);
        mock.xbox_and_minecraft();
        for state in [AccountState::NeedsRefresh, AccountState::Ready] {
            let expires_in = if state == AccountState::Ready {
                Duration::ZERO
            } else {
                Duration::from_secs(3600)
            };
            let stale = account(state, expires_in);
            let secrets = secrets_for(&stale, Some("old-refresh")).await;
            method()
                .refresh(&ctx(&mock), &stale, &secrets, RefreshMode::Recheck)
                .await
                .unwrap();
        }
        assert_eq!(mock.seen_at(TOKEN).len(), 2);
    }

    #[cfg(feature = "loopback")]
    #[tokio::test]
    async fn loopback_listener_receives_the_redirect() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        async fn get(port: u16, target: &str) -> String {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        }

        let mock = MockServices::new();
        mock.on(Method::Post, TOKEN, 200, MS_TOKENS);
        mock.xbox_and_minecraft();
        let ctx = ctx(&mock);
        let options =
            LoginOptions::new().with_interaction(Interaction::Browser(Redirect::Loopback {
                host: "127.0.0.1".to_owned(),
                port: 0,
                path: "/callback".to_owned(),
            }));
        let flow = method().begin(&ctx, &options).await.unwrap();
        let AuthPrompt::OpenUrl { redirect_uri, .. } = flow.prompt().clone() else {
            panic!("expected a browser prompt");
        };
        let port: u16 = redirect_uri
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix("/callback"))
            .unwrap()
            .parse()
            .unwrap();
        let state = prompt_param(flow.prompt(), "state");
        let browser = tokio::spawn(async move {
            let favicon = get(port, "/favicon.ico").await;
            let done = get(port, &format!("/callback?code=C-9&state={state}")).await;
            (favicon, done)
        });
        let update = flow.finish().await.unwrap();
        assert_eq!(update.state, AccountState::Ready);
        let (favicon, done) = browser.await.unwrap();
        assert!(favicon.starts_with("HTTP/1.1 404"));
        assert!(done.starts_with("HTTP/1.1 200") && done.contains("login is complete"));
        assert_eq!(
            form_value(&mock.seen_at(TOKEN)[0].body, "redirect_uri"),
            Some(redirect_uri)
        );
    }

    #[cfg(not(feature = "loopback"))]
    #[tokio::test]
    async fn loopback_needs_its_feature() {
        let mock = MockServices::new();
        let options =
            LoginOptions::new().with_interaction(Interaction::Browser(Redirect::loopback()));
        assert!(matches!(
            method().begin(&ctx(&mock), &options).await,
            Err(AuthError::Unsupported(_))
        ));
    }
}
