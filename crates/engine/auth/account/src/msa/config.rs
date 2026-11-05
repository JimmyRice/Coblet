use std::fmt;

/// Scopes requested from Microsoft: Xbox Live sign-in and a refresh token.
pub const DEFAULT_SCOPES: &str = "XboxLive.signin offline_access";

/// Azure application (client) ID used for Microsoft login.
///
/// coblet has no built-in ID. Each launcher registers its own application
/// (personal Microsoft accounts, public client) and applies for Minecraft
/// services access for it.
///
/// # Examples
///
/// ```
/// use coblet_auth::MsaClientId;
///
/// let id = MsaClientId::new("00000000-0000-0000-0000-000000000000")?;
/// assert_eq!(id.as_str(), "00000000-0000-0000-0000-000000000000");
/// assert!(MsaClientId::new("").is_err());
/// # Ok::<(), coblet_auth::MsaClientIdError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MsaClientId(String);

/// Reason a Client ID was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("msa client id {0:?} must be 1 to 128 characters without whitespace")]
pub struct MsaClientIdError(pub String);

impl MsaClientId {
    /// Validates a Client ID.
    ///
    /// # Errors
    ///
    /// Returns [`MsaClientIdError`] for an empty or too long ID or one with
    /// whitespace or control characters.
    pub fn new(id: impl Into<String>) -> Result<Self, MsaClientIdError> {
        let id = id.into();
        if id.is_empty()
            || id.len() > 128
            || id.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(MsaClientIdError(id));
        }
        Ok(Self(id))
    }

    /// Returns the ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MsaClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MsaClientId(..)")
    }
}

/// Settings of the Microsoft auth method.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MsaConfig {
    /// Azure application ID.
    pub client_id: MsaClientId,
    /// Space-separated OAuth scopes, [`DEFAULT_SCOPES`] by default.
    pub scopes: String,
}

impl MsaConfig {
    /// Creates settings with the default scopes.
    #[must_use]
    pub fn new(client_id: MsaClientId) -> Self {
        Self {
            client_id,
            scopes: DEFAULT_SCOPES.to_owned(),
        }
    }

    /// Requests other scopes, such as `XboxLive.SignIn XboxLive.offline_access`.
    #[must_use]
    pub fn with_scopes(mut self, scopes: impl Into<String>) -> Self {
        self.scopes = scopes.into();
        self
    }
}
