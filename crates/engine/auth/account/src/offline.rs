use coblet_launch_command::{LaunchAuth, offline_uuid};
use coblet_runtime::{AuthPrompt, BoxFuture, Extension};
use coblet_types::{ExtensionId, PlayerName};

use crate::{
    Account, AccountState, AccountUpdate, AuthCtx, AuthError, AuthFlow, AuthMethod, Credentials,
    LaunchSession, LoginOptions, PlayerProfile, RefreshMode, Refreshed,
};

/// ID of the built-in offline method.
pub const OFFLINE_METHOD: &str = "coblet:offline";

/// Offline accounts: a local player name and the offline UUID, no network.
///
/// The login hint of [`LoginOptions`] is the player name. Offline accounts
/// launch with [`LaunchAuth::Offline`] and cannot upload skins.
#[derive(Debug)]
pub struct OfflineMethod {
    id: ExtensionId,
}

impl OfflineMethod {
    /// Creates the method.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: ExtensionId::new(OFFLINE_METHOD).unwrap_or_else(|_| unreachable!()),
        }
    }

    /// Returns the account data of an offline player.
    #[must_use]
    pub fn update_for(name: PlayerName) -> AccountUpdate {
        let uuid = offline_uuid(name.as_str());
        let profile = PlayerProfile::new(name, &uuid).unwrap_or_else(|_| unreachable!());
        AccountUpdate::new(AccountState::Ready).with_profile(profile)
    }
}

impl Default for OfflineMethod {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for OfflineMethod {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &'static str {
        "Offline"
    }
}

impl AuthMethod for OfflineMethod {
    fn begin<'a>(
        &'a self,
        _ctx: &'a AuthCtx,
        options: &'a LoginOptions,
    ) -> BoxFuture<'a, Result<AuthFlow, AuthError>> {
        Box::pin(async move {
            let hint = options.login_hint.clone().ok_or_else(|| {
                AuthError::InvalidOptions("an offline login needs the player name".to_owned())
            })?;
            let update = Self::update_for(PlayerName::new(hint)?);
            Ok(AuthFlow::new(
                AuthPrompt::Nothing,
                async move { Ok(update) },
            ))
        })
    }

    fn refresh<'a>(
        &'a self,
        _ctx: &'a AuthCtx,
        account: &'a Account,
        secrets: &'a Credentials,
        _mode: RefreshMode,
    ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
        Box::pin(async move {
            let name = account.player_name().ok_or(AuthError::NoProfile)?.clone();
            let update = AccountUpdate::from_account(account, secrets.clone());
            Ok(Refreshed::new(
                update,
                Some(LaunchSession::new(LaunchAuth::offline(name))),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_launch_command::LaunchAuth;
    use coblet_runtime::{AuthPrompt, BoxFuture, Request, Response, Transport, TransportError};

    use super::OfflineMethod;
    use crate::{
        Account, AccountId, AuthCtx, AuthError, AuthMethod, Credentials, LoginOptions, RefreshMode,
    };

    struct NoNetwork;

    impl Transport for NoNetwork {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(async { Err(TransportError::Timeout) })
        }
    }

    #[tokio::test]
    async fn logs_in_with_the_offline_uuid() {
        let method = OfflineMethod::new();
        let ctx = AuthCtx::new(Arc::new(NoNetwork));
        let flow = method
            .begin(&ctx, &LoginOptions::new().with_hint("Steve"))
            .await
            .unwrap();
        assert_eq!(flow.prompt(), &AuthPrompt::Nothing);
        let update = flow.finish().await.unwrap();
        let profile = update.profile.clone().unwrap();
        assert_eq!(profile.uuid, "5627dd98e6be3c21b8a8e92344183641");

        let mut account = Account::new(AccountId::generate(), method.id.clone());
        account.profile = Some(profile);
        let refreshed = method
            .refresh(&ctx, &account, &Credentials::new(), RefreshMode::Force)
            .await
            .unwrap();
        let session = refreshed.session.unwrap();
        assert!(matches!(session.auth, LaunchAuth::Offline(ref name) if name.as_str() == "Steve"));
    }

    #[tokio::test]
    async fn rejects_missing_or_bad_names() {
        let method = OfflineMethod::new();
        let ctx = AuthCtx::new(Arc::new(NoNetwork));
        assert!(matches!(
            method.begin(&ctx, &LoginOptions::new()).await,
            Err(AuthError::InvalidOptions(_))
        ));
        assert!(matches!(
            method
                .begin(&ctx, &LoginOptions::new().with_hint("no spaces"))
                .await,
            Err(AuthError::InvalidName(_))
        ));
    }
}
