use coblet_launch_command::{LaunchAuth, offline_uuid};
use coblet_runtime::{AuthPrompt, BoxFuture, Extension};
use coblet_types::{ExtensionId, PlayerName};

use crate::{
    Account, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError, AuthFlow, AuthMethod,
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
        _secrets: &'a AccountSecrets,
        _mode: RefreshMode,
    ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
        Box::pin(async move {
            let name = account.player_name().ok_or(AuthError::NoProfile)?.clone();
            let update = AccountUpdate::from_account(account);
            Ok(Refreshed::new(
                update,
                Some(LaunchSession::new(LaunchAuth::offline(name))),
            ))
        })
    }
}

#[cfg(test)]
#[path = "../tests/offline.rs"]
mod tests;
