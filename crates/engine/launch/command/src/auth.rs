use std::fmt;
use std::fmt::Write as _;

use coblet_types::PlayerName;
use md5::{Digest, Md5};

/// Credential value that never appears in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential(String);

impl Credential {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the value.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Minecraft session from an online login.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlineSession {
    /// Player name.
    pub name: PlayerName,
    /// Profile UUID without dashes.
    pub uuid: String,
    /// Minecraft access token.
    pub access_token: Credential,
    /// Xbox user ID.
    pub xuid: Option<Credential>,
    /// Client ID passed to the game.
    pub client_id: Option<Credential>,
    /// Value of `${user_type}`, such as `mojang` for an authlib-injector
    /// compatible login; `msa` when `None`.
    pub user_type: Option<String>,
}

impl OnlineSession {
    /// Creates a session.
    #[must_use]
    pub fn new(name: PlayerName, uuid: impl Into<String>, access_token: Credential) -> Self {
        Self {
            name,
            uuid: uuid.into(),
            access_token,
            xuid: None,
            client_id: None,
            user_type: None,
        }
    }

    /// Sets `${user_type}`.
    #[must_use]
    pub fn with_user_type(mut self, user_type: impl Into<String>) -> Self {
        self.user_type = Some(user_type.into());
        self
    }
}

/// Identity the game runs with; the three kinds exclude each other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchAuth {
    /// Local name, offline UUID and a placeholder token; needs no network.
    Offline(PlayerName),
    /// Demo mode: like offline, with `is_demo_user` set.
    Demo(PlayerName),
    /// A real session.
    Online(OnlineSession),
}

impl LaunchAuth {
    /// Creates offline authentication.
    #[must_use]
    pub fn offline(name: PlayerName) -> Self {
        Self::Offline(name)
    }

    /// Creates demo authentication.
    #[must_use]
    pub fn demo(name: PlayerName) -> Self {
        Self::Demo(name)
    }

    /// Creates online authentication.
    #[must_use]
    pub fn online(session: OnlineSession) -> Self {
        Self::Online(session)
    }

    /// Returns the player name.
    #[must_use]
    pub fn player_name(&self) -> &PlayerName {
        match self {
            Self::Offline(name) | Self::Demo(name) => name,
            Self::Online(session) => &session.name,
        }
    }

    /// Returns the profile UUID without dashes; offline and demo use [`offline_uuid`].
    #[must_use]
    pub fn uuid(&self) -> String {
        match self {
            Self::Offline(name) | Self::Demo(name) => offline_uuid(name.as_str()),
            Self::Online(session) => session.uuid.clone(),
        }
    }
}

/// Returns the UUID the game derives for an offline player, without dashes.
///
/// It is the name-based (version 3) UUID of `OfflinePlayer:<name>`, the same
/// value Java's `UUID.nameUUIDFromBytes` gives, so servers in offline mode
/// see the same identity.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     coblet_launch_command::offline_uuid("Steve"),
///     "5627dd98e6be3c21b8a8e92344183641"
/// );
/// ```
#[must_use]
pub fn offline_uuid(name: &str) -> String {
    let mut bytes: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}
