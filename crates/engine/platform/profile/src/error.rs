use std::fmt;

use coblet_types::{ExtensionId, Platform};

/// Something a platform lacks to run a version.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Missing {
    /// A library needs native code and no profile provides it for this platform.
    Natives {
        /// The library, such as `org.lwjgl:lwjgl-glfw:3.3.1`.
        library: String,
    },
    /// The Java requirement cannot be met.
    Java {
        /// Java major version needed.
        major: u32,
        /// Why it cannot be met.
        reason: String,
    },
    /// A profile refuses this combination.
    Refused {
        /// The profile.
        profile: ExtensionId,
        /// Its reason.
        reason: String,
    },
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Natives { library } => write!(f, "no native code for {library}"),
            Self::Java { major, reason } => write!(f, "java {major}: {reason}"),
            Self::Refused { profile, reason } => write!(f, "refused by {profile}: {reason}"),
        }
    }
}

/// Failure of the Profile pipeline.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// The platform cannot run the version; nothing was guessed.
    #[error("{} is not supported: {}", .platform_name(platform), .missing.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Unsupported {
        /// The platform.
        platform: Box<Platform>,
        /// What is missing.
        missing: Vec<Missing>,
        /// The pipeline log up to validation, as [`EffectiveLaunchProfile::explain`](crate::EffectiveLaunchProfile::explain) prints it.
        explain: String,
    },
    /// The input is not a complete launch description.
    #[error("incomplete profile: {0}")]
    Incomplete(String),
    /// A data profile is malformed.
    #[error("invalid platform profile {name}: {message}")]
    InvalidProfile {
        /// File name or profile ID.
        name: String,
        /// The parser's description.
        message: String,
    },
    /// A profile tried to change an authentication argument.
    #[error("profile {profile} may not change authentication argument `{argument}`")]
    AuthArgument {
        /// The profile.
        profile: ExtensionId,
        /// The argument.
        argument: String,
    },
}

fn platform_name(platform: &Platform) -> String {
    format!("{} {}", platform.os().as_str(), platform.arch().as_str())
}

impl ProfileError {
    /// Returns what is missing when the platform is unsupported.
    #[must_use]
    pub fn missing(&self) -> &[Missing] {
        match self {
            Self::Unsupported { missing, .. } => missing,
            _ => &[],
        }
    }
}
