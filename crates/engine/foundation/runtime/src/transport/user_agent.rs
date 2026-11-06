use std::fmt;
use std::str::FromStr;

/// HTTP `User-Agent` sent with every request, such as `MyLauncher/1.0 (contact@example.com)`.
///
/// Several services (Modrinth, the PaperMC download service) reject or
/// throttle anonymous clients, so the SDK requires one from its caller. The
/// value must start with a `product/version` token and contain only
/// printable ASCII.
///
/// # Examples
///
/// ```
/// use coblet_runtime::UserAgent;
///
/// let agent = UserAgent::new("MyLauncher/1.0 (contact@example.com)")?;
/// assert_eq!(agent.as_str(), "MyLauncher/1.0 (contact@example.com)");
/// assert!(UserAgent::new("MyLauncher").is_err());
/// # Ok::<(), coblet_runtime::UserAgentError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct UserAgent(String);

/// Reason a user agent was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UserAgentError {
    /// The value does not start with a `product/version` token.
    #[error("user agent `{0}` must start with product/version")]
    MissingProductToken(String),
    /// The value contains a character outside printable ASCII.
    #[error("user agent {0:?} contains a character outside printable ascii")]
    InvalidCharacter(String),
}

impl UserAgent {
    /// Validates a user agent.
    ///
    /// # Errors
    ///
    /// Returns [`UserAgentError::InvalidCharacter`] for anything outside
    /// printable ASCII (which also rules out header injection through CR/LF)
    /// and [`UserAgentError::MissingProductToken`] when the first token is
    /// not `product/version` with both sides non-empty.
    pub fn new(value: impl Into<String>) -> Result<Self, UserAgentError> {
        let value = value.into();
        if !value.chars().all(|c| matches!(c, ' '..='~')) {
            return Err(UserAgentError::InvalidCharacter(value));
        }
        let product = value.split(' ').next().unwrap_or_default();
        let valid_product = product
            .split_once('/')
            .is_some_and(|(name, version)| !name.is_empty() && !version.is_empty());
        if !valid_product {
            return Err(UserAgentError::MissingProductToken(value));
        }
        Ok(Self(value))
    }

    /// Returns the header value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for UserAgent {
    type Err = UserAgentError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::{UserAgent, UserAgentError};

    #[test]
    fn accepts_product_tokens() {
        for value in [
            "a/1",
            "MyLauncher/1.0",
            "MyLauncher/1.0 (contact@example.com) coblet/0.1",
        ] {
            assert_eq!(UserAgent::new(value).unwrap().as_str(), value);
        }
    }

    #[test]
    fn rejects_bad_values() {
        for value in ["", "MyLauncher", "/1.0", "MyLauncher/", " MyLauncher/1.0"] {
            assert!(
                matches!(
                    UserAgent::new(value),
                    Err(UserAgentError::MissingProductToken(_))
                ),
                "{value:?} was accepted"
            );
        }
        assert!(matches!(
            UserAgent::new("MyLauncher/1.0\r\nX-Evil: 1"),
            Err(UserAgentError::InvalidCharacter(_))
        ));
        assert!(matches!(
            UserAgent::new("启动器/1.0"),
            Err(UserAgentError::InvalidCharacter(_))
        ));
    }
}
