use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::ThreadId;

use coblet::platform::{
    Draft, EffectiveLaunchProfile, EnvValue, Natives, NativesSource, ProfileArg, ProfileEdit,
    ProfileError, RuleSettings, Target,
};
use coblet::runtime::Extension;
use coblet::types::{Arch, ArchName, ExtensionId};

use crate::customize::{library, trait_names};
use crate::{CobletError, ErrorKind, LibrarySpec, PlatformInfo};

/// Platform Profile written in Swift, Kotlin or Python.
///
/// It adapts launches to a group of platforms, as profiles written in TOML
/// or Rust do, but decides in code: it can probe the machine, compare
/// versions or pick natives by hand. Register it with
/// `CobletBuilder.add_platform_profile`; it applies to every launch it
/// matches.
///
/// An error from `matches`, `rules` or `rewrite` stops the launch with that
/// message instead of skipping the profile.
#[uniffi::export(foreign)]
pub trait PlatformProfile: Send + Sync {
    /// Returns the ID, such as `acme:haiku`; read once at registration.
    fn id(&self) -> String;

    /// Returns the name shown to users; read once at registration.
    fn display_name(&self) -> String;

    /// Returns the order among third-party profiles; lower values apply
    /// first, and TOML profiles default to 100. Read once at registration.
    fn priority(&self) -> i32;

    /// Reports whether the profile applies to this platform and version.
    ///
    /// # Errors
    ///
    /// An error stops the launch.
    fn matches(&self, target: ProfileTarget) -> Result<bool, CobletError>;

    /// Says how Mojang's rules are evaluated; called only when the profile matches.
    ///
    /// # Errors
    ///
    /// An error stops the launch.
    fn rules(&self, target: ProfileTarget) -> Result<RuleOverrides, CobletError>;

    /// Changes the profile after the rules were evaluated; called only when
    /// the profile matches.
    ///
    /// # Errors
    ///
    /// An error stops the launch.
    fn rewrite(&self, draft: Arc<ProfileDraft>) -> Result<(), CobletError>;
}

/// Component of a version, such as `org.lwjgl3` 3.3.1.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ComponentInfo {
    /// Component UID, such as `net.minecraft` or `org.lwjgl3`.
    pub uid: String,
    /// Component version.
    pub version: String,
}

/// What a profile is matched against: the machine and the version.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ProfileTarget {
    /// The machine.
    pub platform: PlatformInfo,
    /// Version ID, such as `1.20.1`.
    pub game_version: String,
    /// Manifest type, such as `release`, when known.
    pub version_type: Option<String>,
    /// Release time in RFC 3339, when known.
    pub release_time: Option<String>,
    /// Components, such as the game and LWJGL.
    pub components: Vec<ComponentInfo>,
    /// Whether the version uses the legacy argument string.
    pub legacy_arguments: bool,
    /// Version traits, such as `applet`.
    pub traits: Vec<String>,
}

impl ProfileTarget {
    fn from_target(target: &Target<'_>) -> Self {
        let profile = target.profile;
        Self {
            platform: PlatformInfo::from_platform(target.platform),
            game_version: profile.game_version.as_str().to_owned(),
            version_type: profile
                .version_type
                .as_ref()
                .map(|kind| kind.as_str().to_owned()),
            release_time: profile.release_time.map(|time| time.to_string()),
            components: profile
                .components
                .iter()
                .map(|component| ComponentInfo {
                    uid: component.uid.clone(),
                    version: component.version.clone(),
                })
                .collect(),
            legacy_arguments: profile.legacy_arguments,
            traits: trait_names(&profile.traits),
        }
    }
}

/// How Mojang's rules are evaluated; absent fields keep what earlier
/// profiles set. The names match the `[rules]` table of TOML profiles.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct RuleOverrides {
    /// Mojang OS name to evaluate as: `windows`, `osx` or `linux`.
    #[uniffi(default = None)]
    pub mojang_os: Option<String>,
    /// Architecture string to evaluate as.
    #[uniffi(default = None)]
    pub mojang_arch: Option<String>,
    /// OS version to evaluate as.
    #[uniffi(default = None)]
    pub os_version: Option<String>,
    /// Value of `${arch}` in legacy native classifiers (`32` or `64`).
    #[uniffi(default = None)]
    pub natives_arch: Option<String>,
    /// Whether the native code Mojang's rules select runs on this platform.
    #[uniffi(default = None)]
    pub native: Option<bool>,
}

impl RuleOverrides {
    fn apply(self, settings: &mut RuleSettings) {
        let set = |slot: &mut Option<String>, value: Option<String>| {
            if value.is_some() {
                *slot = value;
            }
        };
        set(&mut settings.os_name, self.mojang_os);
        set(&mut settings.os_arch, self.mojang_arch);
        set(&mut settings.os_version, self.os_version);
        set(&mut settings.natives_arch, self.natives_arch);
        if let Some(native) = self.native {
            settings.native = native;
        }
    }
}

/// Where the native code of a library comes from.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NativesChoice {
    /// Keep the natives Mojang's rules selected and vouch for them.
    Artifact,
    /// The natives are not needed on this platform.
    NotNeeded,
    /// The natives are installed on the system in this directory.
    System {
        /// The directory.
        path: String,
    },
    /// The same library with another classifier from a Maven repository.
    Maven {
        /// Repository base URL.
        repository: String,
        /// Classifier, such as `natives-freebsd`.
        classifier: String,
    },
}

/// How the Java runtime may be obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum JavaSourceChoice {
    /// An installed Java or, failing that, a Mojang runtime.
    Any,
    /// Only an installed Java; nothing is downloaded.
    SystemOnly,
}

/// Library of the profile as `rewrite` sees it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LibraryInfo {
    /// Maven coordinate.
    pub name: String,
    /// What `handle_natives` patterns match: the name, plus the native
    /// classifier when the natives are a separate jar.
    pub native_key: String,
    /// Whether the library has native code.
    pub has_natives: bool,
    /// The profile that handled the natives, when one did.
    pub natives_handled_by: Option<String>,
    /// Whether the library still contributes a file.
    pub active: bool,
}

/// Profile under construction, handed to [`PlatformProfile::rewrite`].
///
/// Reads show the profile as it was when `rewrite` started. Changes are
/// applied in order after `rewrite` returns, with the same checks and
/// launch report lines as TOML profiles; a change that is not allowed, such
/// as touching an authentication argument, stops the launch. The draft is
/// valid only during `rewrite`; afterwards every method fails with
/// `InvalidState`.
#[derive(uniffi::Object)]
pub struct ProfileDraft {
    snapshot: EffectiveLaunchProfile,
    edits: Mutex<Option<Vec<ProfileEdit>>>,
}

impl std::fmt::Debug for ProfileDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileDraft").finish_non_exhaustive()
    }
}

fn invalid(message: impl Into<String>) -> CobletError {
    CobletError::new(ErrorKind::InvalidArgument, message)
}

fn absolute(path: String) -> Result<std::path::PathBuf, CobletError> {
    let path = std::path::PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(invalid(format!(
            "native path `{}` must be absolute",
            path.display()
        )))
    }
}

impl ProfileDraft {
    fn new(snapshot: EffectiveLaunchProfile) -> Self {
        Self {
            snapshot,
            edits: Mutex::new(Some(Vec::new())),
        }
    }

    fn open(&self) -> Result<&EffectiveLaunchProfile, CobletError> {
        self.edits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|_| &self.snapshot)
            .ok_or_else(closed)
    }

    fn push(&self, edit: ProfileEdit) -> Result<(), CobletError> {
        self.edits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .ok_or_else(closed)?
            .push(edit);
        Ok(())
    }

    fn close(&self) -> Vec<ProfileEdit> {
        self.edits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_default()
    }
}

fn closed() -> CobletError {
    CobletError::new(ErrorKind::InvalidState, "the rewrite call already returned")
}

fn values(args: &[ProfileArg]) -> Vec<String> {
    args.iter().map(|arg| arg.value.clone()).collect()
}

#[uniffi::export]
impl ProfileDraft {
    /// Returns the libraries.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn libraries(&self) -> Result<Vec<LibraryInfo>, CobletError> {
        Ok(self
            .open()?
            .libraries
            .iter()
            .map(|library| LibraryInfo {
                name: library.name.to_string(),
                native_key: library.native_key(),
                has_natives: library.natives != Natives::None,
                natives_handled_by: match &library.natives {
                    Natives::Handled { by } => Some(by.to_string()),
                    _ => None,
                },
                active: library.is_active(),
            })
            .collect())
    }

    /// Returns the JVM arguments.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn jvm_args(&self) -> Result<Vec<String>, CobletError> {
        Ok(values(&self.open()?.jvm_args))
    }

    /// Returns the game arguments.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn game_args(&self) -> Result<Vec<String>, CobletError> {
        Ok(values(&self.open()?.game_args))
    }

    /// Returns the Java major version the game needs.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn java_major(&self) -> Result<u32, CobletError> {
        Ok(self.open()?.java.major)
    }

    /// Appends a JVM argument unless it is already present.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn append_jvm_arg(&self, value: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::AppendJvmArg(ProfileArg::always(value)))
    }

    /// Appends a game argument unless it is already present.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn append_game_arg(&self, value: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::AppendGameArg(ProfileArg::always(value)))
    }

    /// Removes JVM arguments matching a glob pattern, such as `-Xss*`.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn remove_jvm_args(&self, pattern: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::RemoveJvmArgs(pattern))
    }

    /// Removes game arguments matching a glob pattern.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn remove_game_args(&self, pattern: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::RemoveGameArgs(pattern))
    }

    /// Sets an environment variable.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn set_env(&self, name: String, value: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::SetEnv(name, EnvValue::Set(value)))
    }

    /// Puts directories in front of a path-list variable, such as `LD_LIBRARY_PATH`.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn prepend_env_paths(&self, name: String, paths: Vec<String>) -> Result<(), CobletError> {
        self.push(ProfileEdit::SetEnv(name, EnvValue::PrependPaths(paths)))
    }

    /// Adds a directory with system native libraries.
    ///
    /// # Errors
    ///
    /// Fails for a relative path and after `rewrite` returned.
    pub fn add_native_path(&self, path: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::AddNativePath(absolute(path)?))
    }

    /// Says where the native code of libraries whose
    /// [`LibraryInfo::native_key`] matches a glob pattern comes from, and
    /// marks it handled so the launch accepts it. The first call that
    /// matches a library wins.
    ///
    /// # Errors
    ///
    /// Fails for a relative system path and after `rewrite` returned.
    pub fn handle_natives(
        &self,
        pattern: String,
        choice: NativesChoice,
    ) -> Result<(), CobletError> {
        let source = match choice {
            NativesChoice::Artifact => NativesSource::Artifact,
            NativesChoice::NotNeeded => NativesSource::NotNeeded,
            NativesChoice::System { path } => NativesSource::System(absolute(path)?),
            NativesChoice::Maven {
                repository,
                classifier,
            } => NativesSource::Maven {
                repository,
                classifier,
            },
        };
        self.push(ProfileEdit::HandleNatives { pattern, source })
    }

    /// Adds a library: first when `spec.first` is set, otherwise before the
    /// first library whose name matches the glob `before`, or last.
    ///
    /// # Errors
    ///
    /// Fails for a malformed spec and after `rewrite` returned.
    pub fn add_library(
        &self,
        spec: LibrarySpec,
        before: Option<String>,
    ) -> Result<(), CobletError> {
        let before = if spec.first {
            Some("*".to_owned())
        } else {
            before
        };
        let library = Box::new(library(spec)?);
        self.push(ProfileEdit::AddLibrary { library, before })
    }

    /// Removes libraries whose name matches a glob pattern.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn remove_libraries(&self, pattern: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::RemoveLibraries(pattern))
    }

    /// Sets the Java major version.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn set_java_major(&self, major: u32) -> Result<(), CobletError> {
        self.push(ProfileEdit::JavaMajor(major))
    }

    /// Sets how the Java runtime may be obtained.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn set_java_source(&self, source: JavaSourceChoice) -> Result<(), CobletError> {
        self.push(ProfileEdit::JavaSource(match source {
            JavaSourceChoice::Any => coblet::platform::JavaSource::Any,
            JavaSourceChoice::SystemOnly => coblet::platform::JavaSource::SystemOnly,
        }))
    }

    /// Sets the platform key of the Mojang Java runtime, such as `linux`.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn set_java_runtime(&self, runtime: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::JavaRuntime(runtime))
    }

    /// Sets the architecture the Java runtime must be built for, such as `x86_64`.
    ///
    /// # Errors
    ///
    /// Fails for a malformed name and after `rewrite` returned.
    pub fn set_java_arch(&self, arch: &str) -> Result<(), CobletError> {
        let name = ArchName::new(arch).map_err(|error| invalid(error.to_string()))?;
        self.push(ProfileEdit::JavaArch(Arch::from(name)))
    }

    /// Raises a warning shown in the launch report.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn warn(&self, message: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::Warn(message))
    }

    /// Refuses to launch this combination; the launch fails as unsupported
    /// with this reason.
    ///
    /// # Errors
    ///
    /// Fails after `rewrite` returned.
    pub fn refuse(&self, reason: String) -> Result<(), CobletError> {
        self.push(ProfileEdit::Refuse(reason))
    }
}

/// Registers a foreign profile with the Rust profile engine.
///
/// `matches` and `rules` cannot fail in Rust, so their errors are kept per
/// thread (one resolve runs on one thread) and returned from `rewrite`.
pub(crate) struct ForeignProfile {
    id: ExtensionId,
    display_name: String,
    priority: i32,
    inner: Arc<dyn PlatformProfile>,
    failures: Mutex<HashMap<ThreadId, String>>,
}

impl std::fmt::Debug for ForeignProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForeignProfile")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ForeignProfile {
    pub(crate) fn new(inner: Arc<dyn PlatformProfile>) -> Result<Self, CobletError> {
        let id = ExtensionId::new(inner.id())
            .map_err(|error| invalid(format!("platform profile id: {error}")))?;
        Ok(Self {
            id,
            display_name: inner.display_name(),
            priority: inner.priority(),
            inner,
            failures: Mutex::new(HashMap::new()),
        })
    }

    fn failures(&self) -> std::sync::MutexGuard<'_, HashMap<ThreadId, String>> {
        self.failures.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn failed(&self, message: String) -> ProfileError {
        ProfileError::Failed {
            profile: self.id.clone(),
            message,
        }
    }
}

impl Extension for ForeignProfile {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }
}

impl coblet::platform::PlatformProfile for ForeignProfile {
    fn priority(&self) -> i32 {
        self.priority
    }

    fn matches(&self, target: &Target<'_>) -> bool {
        let thread = std::thread::current().id();
        match self.inner.matches(ProfileTarget::from_target(target)) {
            Ok(matches) => {
                self.failures().remove(&thread);
                matches
            }
            Err(error) => {
                // Matching lets `rewrite` report the error.
                self.failures().insert(thread, error.to_string());
                true
            }
        }
    }

    fn rules(&self, target: &Target<'_>, settings: &mut RuleSettings) {
        let thread = std::thread::current().id();
        if self.failures().contains_key(&thread) {
            return;
        }
        match self.inner.rules(ProfileTarget::from_target(target)) {
            Ok(overrides) => overrides.apply(settings),
            Err(error) => {
                self.failures().insert(thread, error.to_string());
            }
        }
    }

    fn rewrite(&self, draft: &mut Draft<'_>) -> Result<(), ProfileError> {
        if let Some(message) = self.failures().remove(&std::thread::current().id()) {
            return Err(self.failed(message));
        }
        let foreign = Arc::new(ProfileDraft::new(draft.profile().clone()));
        let result = self.inner.rewrite(Arc::clone(&foreign));
        let edits = foreign.close();
        result.map_err(|error| self.failed(error.to_string()))?;
        for edit in edits {
            draft.apply(edit)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
