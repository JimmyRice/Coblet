use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use coblet_runtime::HttpUrl;
use coblet_types::{ExtensionId, PlayerName};

/// Local identifier of an account, unique within one account store.
///
/// IDs use `A-Z a-z 0-9 _ -` and are at most 64 bytes long, so they can be
/// part of file names and secret keys.
///
/// # Examples
///
/// ```
/// use coblet_auth::AccountId;
///
/// let id = AccountId::new("main-account")?;
/// assert_eq!(id.as_str(), "main-account");
/// assert!(AccountId::new("has space").is_err());
/// assert_ne!(AccountId::generate(), AccountId::generate());
/// # Ok::<(), coblet_auth::AccountIdError>(())
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct AccountId(String);

/// Reason an account ID was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccountIdError {
    /// The ID is empty or longer than 64 bytes.
    #[error("account id {0:?} must be 1 to 64 bytes long")]
    Length(String),
    /// The ID contains a character outside `A-Z a-z 0-9 _ -`.
    #[error("account id {0:?} may only contain A-Z a-z 0-9 _ and -")]
    InvalidCharacter(String),
}

impl AccountId {
    /// Validates an ID.
    ///
    /// # Errors
    ///
    /// Returns [`AccountIdError`] for an empty, too long or badly formed ID.
    pub fn new(id: impl Into<String>) -> Result<Self, AccountIdError> {
        let id = id.into();
        if id.is_empty() || id.len() > 64 {
            return Err(AccountIdError::Length(id));
        }
        if !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(AccountIdError::InvalidCharacter(id));
        }
        Ok(Self(id))
    }

    /// Creates a random ID.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    /// Returns the ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AccountId {
    type Err = AccountIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Whether an account can be used to launch right now.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountState {
    /// The account has a valid session or can get one without the user.
    Ready,
    /// The game session has expired; it is renewed without the user before
    /// the next launch.
    NeedsRefresh,
    /// The stored login no longer works and the user has to sign in again.
    NeedsReauth,
    /// The account does not own the game. It can still launch in demo mode.
    NoLicense,
    /// The account owns the game but has not created a player profile yet,
    /// common for Game Pass members.
    NoProfile,
}

impl AccountState {
    /// Returns the stable name used in files, such as `needs-reauth`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NeedsRefresh => "needs-refresh",
            Self::NeedsReauth => "needs-reauth",
            Self::NoLicense => "no-license",
            Self::NoProfile => "no-profile",
        }
    }

    /// Parses a name returned by [`as_str`](Self::as_str).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "ready" => Self::Ready,
            "needs-refresh" => Self::NeedsRefresh,
            "needs-reauth" => Self::NeedsReauth,
            "no-license" => Self::NoLicense,
            "no-profile" => Self::NoProfile,
            _ => return None,
        })
    }
}

impl fmt::Display for AccountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Arm model a skin is drawn for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SkinModel {
    /// Four-pixel arms, the model of Steve.
    #[default]
    Classic,
    /// Three-pixel arms, the model of Alex.
    Slim,
}

impl SkinModel {
    /// Returns the name Minecraft services use: `classic` or `slim`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Slim => "slim",
        }
    }

    /// Parses `classic` or `slim`, ignoring case.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("classic") {
            Some(Self::Classic)
        } else if name.eq_ignore_ascii_case("slim") {
            Some(Self::Slim)
        } else {
            None
        }
    }
}

impl fmt::Display for SkinModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Skin attached to a player profile.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skin {
    /// ID assigned by Minecraft services.
    pub id: String,
    /// Texture URL.
    pub url: HttpUrl,
    /// Arm model.
    pub model: SkinModel,
    /// Whether the player currently wears this skin.
    pub active: bool,
}

impl Skin {
    /// Creates a skin entry.
    #[must_use]
    pub fn new(id: impl Into<String>, url: HttpUrl, model: SkinModel, active: bool) -> Self {
        Self {
            id: id.into(),
            url,
            model,
            active,
        }
    }
}

/// Cape owned by a player.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cape {
    /// ID assigned by Minecraft services, used to select the cape.
    pub id: String,
    /// Name such as `Migrator`, if known.
    pub alias: Option<String>,
    /// Texture URL.
    pub url: HttpUrl,
    /// Whether the player currently shows this cape.
    pub active: bool,
}

impl Cape {
    /// Creates a cape entry.
    #[must_use]
    pub fn new(id: impl Into<String>, alias: Option<String>, url: HttpUrl, active: bool) -> Self {
        Self {
            id: id.into(),
            alias,
            url,
            active,
        }
    }
}

/// Player identity of an account with a snapshot of its skins and capes.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerProfile {
    /// Player name.
    pub name: PlayerName,
    /// Profile UUID: 32 lowercase hexadecimal digits without dashes.
    pub uuid: String,
    /// Skins, at most one of them active.
    pub skins: Vec<Skin>,
    /// Capes the player owns, at most one of them active.
    pub capes: Vec<Cape>,
}

/// Reason a profile UUID was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("profile uuid {0:?} is not 32 hexadecimal digits")]
pub struct ProfileUuidError(pub String);

impl PlayerProfile {
    /// Creates a profile without skins or capes.
    ///
    /// The UUID may contain dashes and uppercase digits; it is stored
    /// without dashes in lowercase.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileUuidError`] when the UUID is not 32 hexadecimal digits.
    pub fn new(name: PlayerName, uuid: &str) -> Result<Self, ProfileUuidError> {
        let simple: String = uuid.chars().filter(|c| *c != '-').collect();
        if simple.len() != 32 || !simple.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ProfileUuidError(uuid.to_owned()));
        }
        Ok(Self {
            name,
            uuid: simple.to_ascii_lowercase(),
            skins: Vec::new(),
            capes: Vec::new(),
        })
    }

    /// Returns the active skin, if the player wears a custom one.
    #[must_use]
    pub fn active_skin(&self) -> Option<&Skin> {
        self.skins.iter().find(|skin| skin.active)
    }

    /// Returns the active cape, if any.
    #[must_use]
    pub fn active_cape(&self) -> Option<&Cape> {
        self.capes.iter().find(|cape| cape.active)
    }
}

/// Account known to the launcher.
///
/// Holds only non-sensitive data. Tokens live in a
/// [`SecretStore`](coblet_runtime::SecretStore) under the names listed in
/// [`secret_names`](Self::secret_names).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// Local ID.
    pub id: AccountId,
    /// Auth method that owns the account, such as `coblet:microsoft`.
    pub method: ExtensionId,
    /// Player profile, missing for [`AccountState::NoProfile`] and most
    /// [`AccountState::NoLicense`] accounts.
    pub profile: Option<PlayerProfile>,
    /// State as of the last login or refresh.
    pub state: AccountState,
    /// When the current game session expires, if the method tracks it.
    pub session_expires_at: Option<SystemTime>,
    /// Non-sensitive values the auth method keeps between refreshes.
    pub data: BTreeMap<String, String>,
    /// Names of the secrets stored for this account.
    pub secret_names: BTreeSet<String>,
}

impl Account {
    /// Creates a ready account without profile, data or secrets.
    #[must_use]
    pub fn new(id: AccountId, method: ExtensionId) -> Self {
        Self {
            id,
            method,
            profile: None,
            state: AccountState::Ready,
            session_expires_at: None,
            data: BTreeMap::new(),
            secret_names: BTreeSet::new(),
        }
    }

    /// Returns the player name, if the account has a profile.
    #[must_use]
    pub fn player_name(&self) -> Option<&PlayerName> {
        self.profile.as_ref().map(|profile| &profile.name)
    }

    /// Returns the state at `now`: a ready account whose session expired is
    /// reported as [`AccountState::NeedsRefresh`].
    #[must_use]
    pub fn state_at(&self, now: SystemTime) -> AccountState {
        match (self.state, self.session_expires_at) {
            (AccountState::Ready, Some(expires)) if expires <= now => AccountState::NeedsRefresh,
            (state, _) => state,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use coblet_types::{ExtensionId, PlayerName};

    use super::{Account, AccountId, AccountState, PlayerProfile, SkinModel};

    #[test]
    fn normalizes_profile_uuids() {
        let name = PlayerName::new("Steve").unwrap();
        let profile =
            PlayerProfile::new(name.clone(), "069A79F4-44E9-4726-A5BE-FCA90E38AAF5").unwrap();
        assert_eq!(profile.uuid, "069a79f444e94726a5befca90e38aaf5");
        assert!(PlayerProfile::new(name.clone(), "069a79f4").is_err());
        assert!(PlayerProfile::new(name, "z69a79f444e94726a5befca90e38aaf5").is_err());
    }

    #[test]
    fn reports_expired_sessions_as_needing_refresh() {
        let mut account = Account::new(
            AccountId::new("a").unwrap(),
            ExtensionId::new("coblet:microsoft").unwrap(),
        );
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        assert_eq!(account.state_at(now), AccountState::Ready);
        account.session_expires_at = Some(now);
        assert_eq!(account.state_at(now), AccountState::NeedsRefresh);
        account.state = AccountState::NoLicense;
        assert_eq!(account.state_at(now), AccountState::NoLicense);
    }

    #[test]
    fn round_trips_names() {
        for state in [
            AccountState::Ready,
            AccountState::NeedsRefresh,
            AccountState::NeedsReauth,
            AccountState::NoLicense,
            AccountState::NoProfile,
        ] {
            assert_eq!(AccountState::parse(state.as_str()), Some(state));
        }
        assert_eq!(SkinModel::parse("SLIM"), Some(SkinModel::Slim));
        assert_eq!(SkinModel::parse("wide"), None);
    }
}
