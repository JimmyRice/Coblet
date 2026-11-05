use std::time::SystemTime;

use coblet_launch_command::{Credential, LaunchAuth};

/// Identity to launch the game with, produced by an auth method.
///
/// Besides the [`LaunchAuth`] it can carry JVM arguments the method needs,
/// such as the `-javaagent` of an authlib-injector compatible login server.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSession {
    /// Offline or online identity.
    pub auth: LaunchAuth,
    /// When the access token expires, for online sessions.
    pub expires_at: Option<SystemTime>,
    /// Extra JVM arguments required by the auth method.
    pub jvm_args: Vec<String>,
}

impl LaunchSession {
    /// Wraps an identity without expiry or extra arguments.
    #[must_use]
    pub fn new(auth: LaunchAuth) -> Self {
        Self {
            auth,
            expires_at: None,
            jvm_args: Vec::new(),
        }
    }

    /// Sets the expiry time.
    #[must_use]
    pub fn with_expiry(mut self, expires_at: SystemTime) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Adds JVM arguments.
    #[must_use]
    pub fn with_jvm_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.jvm_args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Returns the access token of an online session.
    #[must_use]
    pub fn access_token(&self) -> Option<&Credential> {
        match &self.auth {
            LaunchAuth::Online(session) => Some(&session.access_token),
            LaunchAuth::Offline(_) | LaunchAuth::Demo(_) => None,
        }
    }
}
