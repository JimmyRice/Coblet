use std::path::PathBuf;

use coblet_runtime::Extension;
use coblet_types::{ExtensionId, Platform};

use crate::{
    Change, ChangeSource, EffectiveLaunchProfile, EnvValue, PipelineStep, ProfileArg, ProfileError,
    ProfileWarning, ResolvedProfile,
};

/// Layer a profile belongs to; layers apply in this order, after the Mojang data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProfileLayer {
    /// Profiles shipped with coblet.
    Builtin,
    /// Profiles registered by the application or the community.
    #[default]
    ThirdParty,
    /// Overrides from Instance settings.
    Instance,
}

/// What profiles are matched against.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    /// The machine.
    pub platform: &'a Platform,
    /// The version and its components.
    pub profile: &'a ResolvedProfile,
}

/// How the Mojang rules are evaluated, set by profiles in the Evaluate step.
///
/// Without a profile that sets [`os_name`](RuleSettings::os_name), the
/// platform is unknown to Mojang's data and every library with native code
/// is reported missing.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleSettings {
    /// Mojang OS name to evaluate as: `windows`, `osx` or `linux`.
    pub os_name: Option<String>,
    /// Architecture string to evaluate as; defaults to the platform's.
    pub os_arch: Option<String>,
    /// OS version to evaluate as; defaults to the platform's.
    pub os_version: Option<String>,
    /// Value of `${arch}` in legacy native classifiers (`32` or `64`); defaults by architecture.
    pub natives_arch: Option<String>,
    /// Whether the native code Mojang's rules select runs on this platform.
    /// Set by profiles for platforms Mojang ships natives for; under any
    /// other alias every native library must be handled by a profile.
    pub native: bool,
}

/// Platform Profile: adjusts version metadata for a group of platforms.
///
/// Profiles written as data ([`DataProfile`](crate::DataProfile)) and in code
/// implement this trait and are registered with the same
/// [`ProfileEngine`](crate::ProfileEngine).
pub trait PlatformProfile: Extension {
    /// Returns the layer; third-party by default.
    fn layer(&self) -> ProfileLayer {
        ProfileLayer::ThirdParty
    }

    /// Returns the order within the layer; lower values apply first.
    fn priority(&self) -> i32 {
        100
    }

    /// Reports whether the profile applies.
    fn matches(&self, target: &Target<'_>) -> bool;

    /// Reports whether the profile applies, or why it cannot tell.
    ///
    /// The engine calls this instead of [`matches`](Self::matches), which
    /// it calls by default. A profile whose matching fails is skipped with
    /// a warning in the launch report, so a broken profile does not block
    /// platforms it was not written for.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Failed`] when the profile cannot decide.
    fn try_matches(&self, target: &Target<'_>) -> Result<bool, ProfileError> {
        Ok(self.matches(target))
    }

    /// Adjusts how the Mojang rules are evaluated.
    fn rules(&self, target: &Target<'_>, settings: &mut RuleSettings) {
        let _ = (target, settings);
    }

    /// Adjusts how the Mojang rules are evaluated, or fails.
    ///
    /// The engine calls this instead of [`rules`](Self::rules), which it
    /// calls by default. An error stops the resolve: the profile matched,
    /// so the platform depends on it.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Failed`] when the profile cannot decide.
    fn try_rules(
        &self,
        target: &Target<'_>,
        settings: &mut RuleSettings,
    ) -> Result<(), ProfileError> {
        self.rules(target, settings);
        Ok(())
    }

    /// Changes the profile after the rules were evaluated.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError`] when the change is not allowed, such as
    /// touching an authentication argument.
    fn rewrite(&self, draft: &mut Draft<'_>) -> Result<(), ProfileError> {
        let _ = draft;
        Ok(())
    }
}

/// Placeholders and options that belong to authentication; profiles may not add or remove them.
const AUTH_MARKERS: [&str; 8] = [
    "${auth_",
    "${clientid}",
    "${user_type}",
    "${user_properties}",
    "--accessToken",
    "--uuid",
    "--username",
    "--xuid",
];

/// Tells whether an argument carries account data, such as
/// `${auth_access_token}` or `--uuid`.
///
/// Platform Profiles may not add or remove such arguments, and launch
/// customizers may do so only when the launcher allows it.
///
/// # Examples
///
/// ```
/// use coblet_platform::is_auth_argument;
///
/// assert!(is_auth_argument("--accessToken"));
/// assert!(is_auth_argument("-Dname=${auth_player_name}"));
/// assert!(!is_auth_argument("--demo"));
/// ```
#[must_use]
pub fn is_auth_argument(value: &str) -> bool {
    AUTH_MARKERS.iter().any(|marker| value.contains(marker))
}

/// Profile under construction, handed to [`PlatformProfile::rewrite`].
///
/// Every change made through its methods is recorded for
/// [`EffectiveLaunchProfile::explain`].
#[derive(Debug)]
pub struct Draft<'a> {
    pub(crate) profile: &'a mut EffectiveLaunchProfile,
    pub(crate) source: ExtensionId,
    pub(crate) refusals: &'a mut Vec<(ExtensionId, String)>,
}

impl Draft<'_> {
    /// Returns the profile as changed so far.
    #[must_use]
    pub fn profile(&self) -> &EffectiveLaunchProfile {
        self.profile
    }

    /// Returns the profile for changes the other methods do not cover;
    /// record them with [`Draft::note`].
    pub fn profile_mut(&mut self) -> &mut EffectiveLaunchProfile {
        self.profile
    }

    /// Records a change for the explanation.
    pub fn note(&mut self, subject: impl Into<String>, action: impl Into<String>) {
        self.profile.changes.push(Change {
            step: PipelineStep::Rewrite,
            source: ChangeSource::Profile(self.source.clone()),
            subject: subject.into(),
            action: action.into(),
        });
    }

    /// Appends a JVM argument unless the same argument is already present.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::AuthArgument`] for authentication arguments.
    pub fn append_jvm_arg(&mut self, arg: ProfileArg) -> Result<(), ProfileError> {
        self.append(true, arg)
    }

    /// Appends a game argument unless the same argument is already present.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::AuthArgument`] for authentication arguments.
    pub fn append_game_arg(&mut self, arg: ProfileArg) -> Result<(), ProfileError> {
        self.append(false, arg)
    }

    fn append(&mut self, jvm: bool, arg: ProfileArg) -> Result<(), ProfileError> {
        self.check_auth(&arg.value)?;
        let args = if jvm {
            &mut self.profile.jvm_args
        } else {
            &mut self.profile.game_args
        };
        if args.contains(&arg) {
            return Ok(());
        }
        let action = format!("`{}` appended", arg.value);
        args.push(arg);
        self.note(if jvm { "jvm" } else { "game" }, action);
        Ok(())
    }

    /// Removes JVM arguments matching a glob pattern.
    pub fn remove_jvm_args(&mut self, pattern: &str) {
        let removed = remove_matching(&mut self.profile.jvm_args, pattern);
        for value in removed {
            self.note("jvm", format!("`{value}` removed"));
        }
    }

    /// Removes game arguments matching a glob pattern.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::AuthArgument`] when the pattern would remove an
    /// authentication argument; nothing is removed then.
    pub fn remove_game_args(&mut self, pattern: &str) -> Result<(), ProfileError> {
        if let Some(arg) =
            self.profile.game_args.iter().find(|arg| {
                crate::glob_matches(pattern, &arg.value) && is_auth_argument(&arg.value)
            })
        {
            return Err(ProfileError::AuthArgument {
                profile: self.source.clone(),
                argument: arg.value.clone(),
            });
        }
        let removed = remove_matching(&mut self.profile.game_args, pattern);
        for value in removed {
            self.note("game", format!("`{value}` removed"));
        }
        Ok(())
    }

    fn check_auth(&self, value: &str) -> Result<(), ProfileError> {
        if is_auth_argument(value) {
            return Err(ProfileError::AuthArgument {
                profile: self.source.clone(),
                argument: value.to_owned(),
            });
        }
        Ok(())
    }

    /// Sets an environment variable.
    pub fn set_env(&mut self, name: impl Into<String>, value: EnvValue) {
        let name = name.into();
        self.note(format!("env {name}"), format!("set to {value:?}"));
        self.profile.env.insert(name, value);
    }

    /// Adds a directory with system native libraries.
    pub fn add_native_path(&mut self, path: PathBuf) {
        if self.profile.native_paths.contains(&path) {
            return;
        }
        self.note("native path", format!("`{}` added", path.display()));
        self.profile.native_paths.push(path);
    }

    /// Raises a warning shown before launch.
    pub fn warn(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.note("warning", format!("`{message}`"));
        self.profile.warnings.push(ProfileWarning {
            profile: self.source.clone(),
            message,
        });
    }

    /// Refuses to launch this combination; validation reports the reason.
    pub fn refuse(&mut self, reason: impl Into<String>) {
        self.refusals.push((self.source.clone(), reason.into()));
    }

    /// Returns the ID of the profile making changes.
    #[must_use]
    pub fn source(&self) -> &ExtensionId {
        &self.source
    }
}

fn remove_matching(args: &mut Vec<ProfileArg>, pattern: &str) -> Vec<String> {
    let mut removed = Vec::new();
    args.retain(|arg| {
        let matches = crate::glob_matches(pattern, &arg.value);
        if matches {
            removed.push(arg.value.clone());
        }
        !matches
    });
    removed
}
