use std::collections::HashMap;
use std::time::SystemTime;

use coblet::auth::{Account, AccountId, Cape, Interaction, PlayerProfile, Redirect, Skin};
use coblet::runtime::AuthPrompt;

use crate::{CobletError, ErrorKind};

/// Whether an account can launch right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AccountState {
    /// The account has a session or can get one without the user.
    Ready,
    /// The game session expired; it is renewed before the next launch.
    NeedsRefresh,
    /// The stored login no longer works; the user has to sign in again.
    NeedsReauth,
    /// The account does not own the game; it can still launch in demo mode.
    NoLicense,
    /// The account owns the game but has no player profile yet.
    NoProfile,
}

impl From<coblet::auth::AccountState> for AccountState {
    fn from(state: coblet::auth::AccountState) -> Self {
        use coblet::auth::AccountState as State;
        match state {
            State::NeedsRefresh => Self::NeedsRefresh,
            State::NeedsReauth => Self::NeedsReauth,
            State::NoLicense => Self::NoLicense,
            State::NoProfile => Self::NoProfile,
            _ => Self::Ready,
        }
    }
}

impl From<AccountState> for coblet::auth::AccountState {
    fn from(state: AccountState) -> Self {
        match state {
            AccountState::Ready => Self::Ready,
            AccountState::NeedsRefresh => Self::NeedsRefresh,
            AccountState::NeedsReauth => Self::NeedsReauth,
            AccountState::NoLicense => Self::NoLicense,
            AccountState::NoProfile => Self::NoProfile,
        }
    }
}

/// What an account's entitlements said about the game at its last check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameLicense {
    /// The account bought the game.
    Purchased,
    /// The account may play without a purchase on record, for example
    /// through a subscription.
    Entitled,
    /// The account may not play the full game.
    Missing,
}

impl From<coblet::auth::GameLicense> for GameLicense {
    fn from(license: coblet::auth::GameLicense) -> Self {
        use coblet::auth::GameLicense as License;
        match license {
            License::Purchased => Self::Purchased,
            License::Entitled => Self::Entitled,
            _ => Self::Missing,
        }
    }
}

/// Arm width of a skin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SkinModel {
    /// Arms 4 pixels wide, called Steve or wide.
    Classic,
    /// Arms 3 pixels wide, called Alex or slim.
    Slim,
}

impl From<coblet::auth::SkinModel> for SkinModel {
    fn from(model: coblet::auth::SkinModel) -> Self {
        match model {
            coblet::auth::SkinModel::Slim => Self::Slim,
            coblet::auth::SkinModel::Classic => Self::Classic,
        }
    }
}

impl From<SkinModel> for coblet::auth::SkinModel {
    fn from(model: SkinModel) -> Self {
        match model {
            SkinModel::Classic => Self::Classic,
            SkinModel::Slim => Self::Slim,
        }
    }
}

/// Skin uploaded to an account.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SkinInfo {
    /// ID given by Minecraft services.
    pub id: String,
    /// Texture URL.
    pub url: String,
    /// Arm width.
    pub model: SkinModel,
    /// Whether the player wears it.
    pub active: bool,
}

impl From<&Skin> for SkinInfo {
    fn from(skin: &Skin) -> Self {
        Self {
            id: skin.id.clone(),
            url: skin.url.as_str().to_owned(),
            model: skin.model.into(),
            active: skin.active,
        }
    }
}

/// Cape owned by an account.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CapeInfo {
    /// ID to pass to `Coblet.set_cape`.
    pub id: String,
    /// Name such as `Migrator`, when known.
    pub alias: Option<String>,
    /// Texture URL.
    pub url: String,
    /// Whether the player shows it.
    pub active: bool,
}

impl From<&Cape> for CapeInfo {
    fn from(cape: &Cape) -> Self {
        Self {
            id: cape.id.clone(),
            alias: cape.alias.clone(),
            url: cape.url.as_str().to_owned(),
            active: cape.active,
        }
    }
}

/// Player profile of an account: name, UUID, skins and capes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PlayerInfo {
    /// Player name.
    pub name: String,
    /// UUID as 32 lowercase hex digits.
    pub uuid: String,
    /// Uploaded skins; empty for the default skin.
    pub skins: Vec<SkinInfo>,
    /// Owned capes.
    pub capes: Vec<CapeInfo>,
}

impl From<&PlayerProfile> for PlayerInfo {
    fn from(profile: &PlayerProfile) -> Self {
        Self {
            name: profile.name.as_str().to_owned(),
            uuid: profile.uuid.clone(),
            skins: profile.skins.iter().map(SkinInfo::from).collect(),
            capes: profile.capes.iter().map(CapeInfo::from).collect(),
        }
    }
}

impl TryFrom<PlayerInfo> for PlayerProfile {
    type Error = CobletError;

    fn try_from(info: PlayerInfo) -> Result<Self, CobletError> {
        let invalid = |error: &dyn std::fmt::Display| {
            CobletError::new(ErrorKind::InvalidArgument, format!("player: {error}"))
        };
        let url = |url: String| coblet::runtime::HttpUrl::new(url).map_err(|error| invalid(&error));
        let name = coblet::types::PlayerName::new(info.name).map_err(|error| invalid(&error))?;
        let mut profile = Self::new(name, &info.uuid).map_err(|error| invalid(&error))?;
        for skin in info.skins {
            profile.skins.push(Skin::new(
                skin.id,
                url(skin.url)?,
                skin.model.into(),
                skin.active,
            ));
        }
        for cape in info.capes {
            profile
                .capes
                .push(Cape::new(cape.id, cape.alias, url(cape.url)?, cape.active));
        }
        Ok(profile)
    }
}

/// Saved account.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AccountInfo {
    /// Account ID, stable across logins.
    pub id: String,
    /// Auth method, such as `coblet:microsoft` or `coblet:offline`.
    pub method: String,
    /// Whether it can launch now.
    pub state: AccountState,
    /// Player profile, absent for accounts that have none yet.
    pub player: Option<PlayerInfo>,
    /// When the current game session expires, when there is one.
    pub session_expires_at: Option<SystemTime>,
    /// Result of the last license check; absent for offline accounts and
    /// accounts never checked, such as imported ones.
    pub license: Option<GameLicense>,
    /// When `license` was read.
    pub license_checked_at: Option<SystemTime>,
    /// Whether the last check found that the account bought the game, like
    /// Prism Launcher's "owns Minecraft". False for subscriptions.
    pub owns_game: bool,
    /// Non-sensitive values its auth method keeps, such as the address of
    /// a login server.
    pub data: HashMap<String, String>,
}

impl From<&Account> for AccountInfo {
    fn from(account: &Account) -> Self {
        Self {
            id: account.id.as_str().to_owned(),
            method: account.method.as_str().to_owned(),
            state: account.state.into(),
            player: account.profile.as_ref().map(PlayerInfo::from),
            session_expires_at: account.session_expires_at,
            license: account.license.map(|check| check.license.into()),
            license_checked_at: account.license.map(|check| check.checked_at),
            owns_game: account.owns_game(),
            data: account.data.clone().into_iter().collect(),
        }
    }
}

/// How the user signs in to Microsoft.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LoginInteraction {
    /// Show a code that the user enters at a Microsoft page, on any device.
    DeviceCode,
    /// Open a sign-in page; a built-in listener on `127.0.0.1` with a free
    /// port receives the redirect.
    Browser,
    /// Like `Browser`, with the redirect URI parts chosen: `host` such as
    /// `127.0.0.1` or `localhost`, `port` (0 for a free one) and `path`.
    BrowserLoopback {
        /// Host of the redirect URI.
        host: String,
        /// Port, or 0.
        port: u16,
        /// Path, starting with `/`.
        path: String,
    },
    /// Open a sign-in page that redirects to `redirect_uri`, such as a
    /// custom URL scheme of the launcher; pass the full URI the launcher
    /// receives to `LoginFlow.submit_redirect`.
    BrowserRedirect {
        /// Redirect URI registered for the Client ID.
        redirect_uri: String,
    },
}

impl From<LoginInteraction> for Interaction {
    fn from(interaction: LoginInteraction) -> Self {
        match interaction {
            LoginInteraction::DeviceCode => Self::DeviceCode,
            LoginInteraction::Browser => Self::Browser(Redirect::loopback()),
            LoginInteraction::BrowserLoopback { host, port, path } => {
                Self::Browser(Redirect::Loopback { host, port, path })
            }
            LoginInteraction::BrowserRedirect { redirect_uri } => {
                Self::Browser(Redirect::External { redirect_uri })
            }
        }
    }
}

/// What the user has to do to finish a login.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LoginPrompt {
    /// Open `verification_uri` and enter `user_code` before `expires_at`.
    DeviceCode {
        /// Code to enter.
        user_code: String,
        /// Page to enter it at.
        verification_uri: String,
        /// When the code stops working.
        expires_at: SystemTime,
        /// Instruction text from Microsoft, when given.
        message: Option<String>,
    },
    /// Open `url` in a browser; the login continues once the browser
    /// reaches `redirect_uri`.
    OpenUrl {
        /// Sign-in page.
        url: String,
        /// Where the browser is sent afterwards.
        redirect_uri: String,
    },
    /// Nothing; the login finishes on its own.
    Nothing,
}

impl From<&AuthPrompt> for LoginPrompt {
    fn from(prompt: &AuthPrompt) -> Self {
        match prompt {
            AuthPrompt::DeviceCode {
                user_code,
                verification_uri,
                expires_at,
                message,
            } => Self::DeviceCode {
                user_code: user_code.clone(),
                verification_uri: verification_uri.as_str().to_owned(),
                expires_at: *expires_at,
                message: message.clone(),
            },
            AuthPrompt::OpenUrl { url, redirect_uri } => Self::OpenUrl {
                url: url.as_str().to_owned(),
                redirect_uri: redirect_uri.clone(),
            },
            _ => Self::Nothing,
        }
    }
}

pub(crate) fn account_id(id: &str) -> Result<AccountId, CobletError> {
    AccountId::new(id)
        .map_err(|error| CobletError::new(ErrorKind::InvalidArgument, error.to_string()))
}
