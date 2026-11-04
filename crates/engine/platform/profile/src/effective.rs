use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::PathBuf;

use coblet_catalog::Timestamp;
use coblet_mojang::{
    AssetIndexRef, Condition, Download, LoggingConfig, RuleContext, VersionTraits,
};
use coblet_types::{
    Arch, ExtensionId, GameVersionId, MavenCoordinate, Platform, Sha1Digest, VersionType,
};

/// Where a library file comes from.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileSource {
    /// A fixed address; the hash and size are checked when known.
    Download {
        /// The address.
        url: String,
        /// SHA-1 of the file.
        sha1: Option<Sha1Digest>,
        /// Size in bytes.
        size: Option<u64>,
    },
    /// A Maven repository file whose SHA-1 is published next to it as `<url>.sha1`.
    Sidecar {
        /// The address of the file.
        url: String,
    },
    /// No address: the file must already be in the libraries directory,
    /// for example a jar produced by a Mod Loader installer.
    Existing,
}

/// File in the `libraries` directory.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFile {
    /// Path relative to the `libraries` directory, with `/` separators.
    pub path: String,
    /// Where to get it.
    pub source: FileSource,
}

impl LibraryFile {
    /// Creates a file entry.
    #[must_use]
    pub fn new(path: impl Into<String>, source: FileSource) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }
}

/// Native jar extracted into the natives directory before launch (versions before 1.19).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeJar {
    /// The jar.
    pub file: LibraryFile,
    /// Classifier of the jar, such as `natives-osx`.
    pub classifier: String,
    /// Path prefixes left out when extracting.
    pub exclude: Vec<String>,
}

/// Whether a library carries native code, and who vouches for it on this platform.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Natives {
    /// The library has no native code.
    #[default]
    None,
    /// Native code chosen by Mojang's rules for the platform the rules were evaluated as.
    Mojang,
    /// Native code provided, replaced or declared unnecessary by a profile.
    Handled {
        /// The profile.
        by: ExtensionId,
    },
}

/// Library of the effective profile.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveLibrary {
    /// Maven coordinate as listed in the metadata.
    pub name: MavenCoordinate,
    /// Jar on the classpath, if any.
    pub classpath: Option<LibraryFile>,
    /// Native jar to extract, if any.
    pub extract: Option<NativeJar>,
    /// Native code status.
    pub natives: Natives,
}

impl EffectiveLibrary {
    /// Creates a library without native code.
    #[must_use]
    pub fn new(name: MavenCoordinate, classpath: Option<LibraryFile>) -> Self {
        Self {
            name,
            classpath,
            extract: None,
            natives: Natives::None,
        }
    }

    /// Returns the text native rules match against: the coordinate, with the
    /// native classifier appended for libraries that extract a native jar.
    #[must_use]
    pub fn native_key(&self) -> String {
        match &self.extract {
            Some(jar) => format!("{}:{}", self.name, jar.classifier),
            None => self.name.to_string(),
        }
    }

    /// Reports whether the library still contributes a file.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.classpath.is_some() || self.extract.is_some()
    }
}

/// Argument with the launch features it depends on.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileArg {
    /// The argument template, such as `--username` or `${auth_player_name}`.
    pub value: String,
    /// Launch features that decide whether it is passed; OS conditions are already decided.
    pub condition: Condition,
}

impl ProfileArg {
    /// Creates an argument that is always passed.
    #[must_use]
    pub fn always(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            condition: Condition::Always,
        }
    }
}

/// Value of an environment variable set for the game.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvValue {
    /// Replace the variable.
    Set(String),
    /// Add directories to a search path variable, keeping the inherited value after them.
    PrependPaths(Vec<String>),
}

/// How the Java runtime may be obtained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum JavaSource {
    /// An installed Java or, failing that, the Mojang runtime for [`JavaRequirement::runtime_platform`].
    #[default]
    Any,
    /// Only an installed Java; nothing is downloaded.
    SystemOnly,
}

/// Java runtime needed by the game on this platform.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaRequirement {
    /// Java major version.
    pub major: u32,
    /// Mojang runtime component, such as `jre-legacy`.
    pub component: Option<String>,
    /// Platform key in the Mojang runtime index, such as `mac-os-arm64`; `None` when Mojang has no runtime here.
    pub runtime_platform: Option<String>,
    /// How the runtime may be obtained.
    pub source: JavaSource,
    /// Architecture the runtime must be built for; differs from the
    /// machine's when the game runs under emulation such as Rosetta.
    pub arch: Arch,
}

/// Pipeline step that made a change, for [`EffectiveLaunchProfile::explain`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PipelineStep {
    /// Reading the version metadata.
    Raw,
    /// Selecting Platform Profiles.
    Match,
    /// Evaluating Mojang rules.
    Evaluate,
    /// Applying profile changes.
    Rewrite,
    /// Checking the result.
    Validate,
}

impl fmt::Display for PipelineStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Raw => "raw",
            Self::Match => "match",
            Self::Evaluate => "evaluate",
            Self::Rewrite => "rewrite",
            Self::Validate => "validate",
        })
    }
}

/// Who made a change.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChangeSource {
    /// The version metadata.
    Metadata,
    /// A Platform Profile.
    Profile(ExtensionId),
}

impl fmt::Display for ChangeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Metadata => f.write_str("metadata"),
            Self::Profile(id) => write!(f, "{id}"),
        }
    }
}

/// One line of the explanation: who changed what at which step.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The step.
    pub step: PipelineStep,
    /// The source.
    pub source: ChangeSource,
    /// What was changed, such as `library org.lwjgl:lwjgl:3.3.1` or `jvm`.
    pub subject: String,
    /// What happened.
    pub action: String,
}

/// Warning raised by a profile, such as running under Rosetta.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileWarning {
    /// The profile.
    pub profile: ExtensionId,
    /// The message.
    pub message: String,
}

/// Launch description for one platform: rules decided, profiles applied.
///
/// Arguments keep the launch-feature conditions (demo mode, custom
/// resolution, Quick Play), decided by the launch options.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveLaunchProfile {
    /// The platform.
    pub platform: Platform,
    /// Game version.
    pub game_version: GameVersionId,
    /// Release channel.
    pub version_type: Option<VersionType>,
    /// Release time.
    pub release_time: Option<Timestamp>,
    /// Java class with the `main` method.
    pub main_class: String,
    /// Libraries in classpath order, including inactive ones for explanation.
    pub libraries: Vec<EffectiveLibrary>,
    /// The client jar.
    pub client: Option<Download>,
    /// Directories with system native libraries, added to the native search path.
    pub native_paths: Vec<PathBuf>,
    /// JVM arguments.
    pub jvm_args: Vec<ProfileArg>,
    /// Game arguments.
    pub game_args: Vec<ProfileArg>,
    /// Environment variables.
    pub env: BTreeMap<String, EnvValue>,
    /// Java requirement.
    pub java: JavaRequirement,
    /// Asset index to use.
    pub asset_index: Option<AssetIndexRef>,
    /// Asset index ID.
    pub assets: String,
    /// Log configuration.
    pub logging: Option<LoggingConfig>,
    /// Version traits.
    pub traits: VersionTraits,
    /// Context the Mojang rules were evaluated with.
    pub rule_context: RuleContext,
    /// Applied profiles, in order.
    pub profiles: Vec<ExtensionId>,
    /// Warnings raised by profiles.
    pub warnings: Vec<ProfileWarning>,
    /// Change log.
    pub changes: Vec<Change>,
}

impl EffectiveLaunchProfile {
    /// Iterates over the jars on the classpath, in order.
    pub fn classpath(&self) -> impl Iterator<Item = &LibraryFile> {
        self.libraries
            .iter()
            .filter_map(|library| library.classpath.as_ref())
    }

    /// Iterates over the native jars to extract.
    pub fn native_jars(&self) -> impl Iterator<Item = &NativeJar> {
        self.libraries
            .iter()
            .filter_map(|library| library.extract.as_ref())
    }

    /// Returns where every library, argument, environment variable and the
    /// Java requirement came from, one change per line, in pipeline order.
    #[must_use]
    pub fn explain(&self) -> String {
        explain(&self.platform, &self.profiles, &self.changes)
    }
}

pub(crate) fn explain(platform: &Platform, profiles: &[ExtensionId], changes: &[Change]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "platform: {} {}{}",
        platform.os().as_str(),
        platform.arch().as_str(),
        platform
            .os_version()
            .map(|version| format!(" {version}"))
            .unwrap_or_default()
    );
    let names: Vec<&str> = profiles.iter().map(ExtensionId::as_str).collect();
    let _ = writeln!(
        out,
        "profiles: {}",
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    );
    let mut ordered: Vec<&Change> = changes.iter().collect();
    ordered.sort_by_key(|change| change.step);
    for change in ordered {
        let _ = writeln!(
            out,
            "[{}] {}: {} {}",
            change.step, change.source, change.subject, change.action
        );
    }
    out
}
