use std::collections::BTreeMap;
use std::path::PathBuf;

use coblet_catalog::Timestamp;
use coblet_mojang::{Condition, FeatureClause, RuleAction, VersionTrait};
use coblet_runtime::Extension;
use coblet_types::{Arch, ExtensionId, MavenCoordinate};
use serde::Deserialize;

use crate::{
    Draft, EffectiveLibrary, EnvValue, FileSource, JavaMatch, JavaSource, LibraryFile,
    NativesSource, PlatformProfile, ProfileArg, ProfileError, ProfileLayer, RuleSettings, Target,
    compare_versions, glob_matches,
};

/// Platform Profile written as TOML.
///
/// # Examples
///
/// ```
/// use coblet_platform::{DataProfile, PlatformProfile, ProfileLayer};
/// use coblet_runtime::Extension;
///
/// let profile = DataProfile::from_toml(r#"
///     id = "community:freebsd-lwjgl3"
///     priority = 200
///
///     [match]
///     os = ["freebsd"]
///     arch = ["x86_64", "aarch64"]
///     requires_component = { uid = "org.lwjgl3" }
///
///     [rules]
///     mojang_os = "linux"
///
///     [[natives]]
///     match = "org.lwjgl:*:*:natives-linux*"
///     source = { system = "/usr/local/lib" }
///
///     [jvm]
///     append = ["-Dorg.lwjgl.librarypath=/usr/local/lib"]
///
///     [java]
///     source = "system-only"
/// "#, ProfileLayer::ThirdParty)?;
/// assert_eq!(profile.id().as_str(), "community:freebsd-lwjgl3");
/// assert_eq!(profile.priority(), 200);
/// # Ok::<(), coblet_platform::ProfileError>(())
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct DataProfile {
    file: ProfileFile,
    layer: ProfileLayer,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    id: ExtensionId,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default = "default_priority")]
    priority: i32,
    #[serde(rename = "match", default)]
    when: MatchSpec,
    #[serde(default)]
    rules: RulesSpec,
    #[serde(default)]
    libraries: Vec<LibrarySpec>,
    #[serde(default)]
    natives: Vec<NativesSpec>,
    #[serde(default)]
    jvm: ArgsSpec,
    #[serde(default)]
    game: ArgsSpec,
    #[serde(default)]
    env: EnvSpec,
    #[serde(default)]
    java: JavaSpec,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    refuse: Option<String>,
}

fn default_priority() -> i32 {
    100
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchSpec {
    #[serde(default)]
    os: Vec<String>,
    #[serde(default)]
    arch: Vec<String>,
    #[serde(default)]
    libc: Vec<String>,
    #[serde(default)]
    os_version: Option<RangeSpec>,
    #[serde(default)]
    platform_traits: Vec<String>,
    #[serde(default)]
    requires_component: Option<ComponentSpec>,
    #[serde(default)]
    lacks_component: Option<ComponentSpec>,
    #[serde(default)]
    legacy_arguments: Option<bool>,
    #[serde(default)]
    released_after: Option<Timestamp>,
    #[serde(default)]
    released_before: Option<Timestamp>,
    #[serde(default)]
    version_traits: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeSpec {
    #[serde(default)]
    min: Option<String>,
    #[serde(default)]
    below: Option<String>,
}

impl RangeSpec {
    fn contains(&self, version: &str) -> bool {
        self.min
            .as_deref()
            .is_none_or(|min| compare_versions(version, min).is_ge())
            && self
                .below
                .as_deref()
                .is_none_or(|below| compare_versions(version, below).is_lt())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentSpec {
    uid: String,
    #[serde(default, flatten)]
    version: RangeSpec,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesSpec {
    #[serde(default)]
    mojang_os: Option<String>,
    #[serde(default)]
    mojang_arch: Option<String>,
    #[serde(default)]
    os_version: Option<String>,
    #[serde(default)]
    natives_arch: Option<String>,
    #[serde(default)]
    native: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LibrarySpec {
    #[serde(rename = "match", default)]
    pattern: Option<String>,
    #[serde(default)]
    remove: bool,
    #[serde(default)]
    add: Option<AddLibrary>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddLibrary {
    name: MavenCoordinate,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    sha1: Option<coblet_types::Sha1Digest>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativesSpec {
    #[serde(rename = "match")]
    pattern: String,
    source: NativesSpecSource,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum NativesSpecSource {
    Keyword(NativesKeyword),
    System { system: PathBuf },
    Maven { maven: String, classifier: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NativesKeyword {
    /// Keep the downloaded natives and vouch for them.
    Artifact,
    /// The natives are not needed on this platform.
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArgsSpec {
    #[serde(default)]
    append: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    conditional: Vec<ConditionalArgs>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConditionalArgs {
    features: BTreeMap<String, bool>,
    values: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvSpec {
    #[serde(default)]
    set: BTreeMap<String, String>,
    #[serde(default)]
    prepend_path: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct JavaSpec {
    #[serde(default)]
    major: Option<u32>,
    #[serde(default)]
    source: Option<JavaSourceSpec>,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    arch: Option<String>,
    #[serde(default, rename = "match")]
    matching: Option<JavaMatchSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum JavaMatchSpec {
    Exact,
    AtLeast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum JavaSourceSpec {
    Any,
    SystemOnly,
}

impl DataProfile {
    /// Parses a profile.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::InvalidProfile`] for malformed files or unknown keys.
    pub fn from_toml(text: &str, layer: ProfileLayer) -> Result<Self, ProfileError> {
        let file: ProfileFile =
            toml::from_str(text).map_err(|error| ProfileError::InvalidProfile {
                name: text
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("id"))
                    .map_or_else(
                        || "<unnamed>".to_owned(),
                        |id| {
                            id.trim_start_matches([' ', '='])
                                .trim_matches('"')
                                .to_owned()
                        },
                    ),
                message: error.message().to_owned(),
            })?;
        Ok(Self { file, layer })
    }
}

impl Extension for DataProfile {
    fn id(&self) -> &ExtensionId {
        &self.file.id
    }

    fn display_name(&self) -> &str {
        self.file
            .display_name
            .as_deref()
            .unwrap_or_else(|| self.file.id.as_str())
    }
}

fn any_matches(allowed: &[String], value: &str) -> bool {
    allowed.is_empty() || allowed.iter().any(|item| item == value)
}

impl PlatformProfile for DataProfile {
    fn layer(&self) -> ProfileLayer {
        self.layer
    }

    fn priority(&self) -> i32 {
        self.file.priority
    }

    fn matches(&self, target: &Target<'_>) -> bool {
        let when = &self.file.when;
        let platform = target.platform;
        let profile = target.profile;
        if !any_matches(&when.os, platform.os().as_str())
            || !any_matches(&when.arch, platform.arch().as_str())
        {
            return false;
        }
        if !when.libc.is_empty()
            && !platform
                .libc()
                .is_some_and(|libc| any_matches(&when.libc, libc.as_str()))
        {
            return false;
        }
        if let Some(range) = &when.os_version
            && !platform
                .os_version()
                .is_some_and(|version| range.contains(version))
        {
            return false;
        }
        if !when
            .platform_traits
            .iter()
            .all(|name| platform.has_trait(name))
        {
            return false;
        }
        if let Some(spec) = &when.requires_component
            && !profile
                .component(&spec.uid)
                .is_some_and(|component| spec.version.contains(&component.version))
        {
            return false;
        }
        if let Some(spec) = &when.lacks_component
            && profile
                .component(&spec.uid)
                .is_some_and(|component| spec.version.contains(&component.version))
        {
            return false;
        }
        if when
            .legacy_arguments
            .is_some_and(|legacy| legacy != profile.legacy_arguments)
        {
            return false;
        }
        if when.released_after.is_some() || when.released_before.is_some() {
            let Some(released) = profile.release_time else {
                return false;
            };
            if when.released_after.is_some_and(|after| released <= after)
                || when
                    .released_before
                    .is_some_and(|before| released >= before)
            {
                return false;
            }
        }
        when.version_traits
            .iter()
            .all(|name| profile.traits.contains(&VersionTrait::from(name.as_str())))
    }

    fn rules(&self, _target: &Target<'_>, settings: &mut RuleSettings) {
        let rules = &self.file.rules;
        if let Some(name) = &rules.mojang_os {
            settings.os_name = Some(name.clone());
        }
        if let Some(arch) = &rules.mojang_arch {
            settings.os_arch = Some(arch.clone());
        }
        if let Some(version) = &rules.os_version {
            settings.os_version = Some(version.clone());
        }
        if let Some(arch) = &rules.natives_arch {
            settings.natives_arch = Some(arch.clone());
        }
        if let Some(native) = rules.native {
            settings.native = native;
        }
    }

    fn rewrite(&self, draft: &mut Draft<'_>) -> Result<(), ProfileError> {
        let file = &self.file;
        if let Some(reason) = &file.refuse {
            draft.refuse(reason.clone());
        }
        for message in &file.warnings {
            draft.warn(message.clone());
        }
        for spec in &file.libraries {
            apply_library(draft, spec);
        }
        if !file.natives.is_empty() {
            apply_natives(self, draft);
        }
        for pattern in &file.jvm.remove {
            draft.remove_jvm_args(pattern)?;
        }
        for value in &file.jvm.append {
            draft.append_jvm_arg(ProfileArg::always(value.clone()))?;
        }
        for group in &file.jvm.conditional {
            for value in &group.values {
                draft.append_jvm_arg(conditional(value, &group.features))?;
            }
        }
        for pattern in &file.game.remove {
            draft.remove_game_args(pattern)?;
        }
        for value in &file.game.append {
            draft.append_game_arg(ProfileArg::always(value.clone()))?;
        }
        for group in &file.game.conditional {
            for value in &group.values {
                draft.append_game_arg(conditional(value, &group.features))?;
            }
        }
        for (name, value) in &file.env.set {
            draft.set_env(name.clone(), EnvValue::Set(value.clone()));
        }
        for (name, paths) in &file.env.prepend_path {
            draft.set_env(name.clone(), EnvValue::PrependPaths(paths.clone()));
        }
        apply_java(draft, &file.java);
        Ok(())
    }
}

fn conditional(value: &str, features: &BTreeMap<String, bool>) -> ProfileArg {
    let mut arg = ProfileArg::always(value);
    arg.condition = Condition::Features(vec![FeatureClause::new(
        RuleAction::Allow,
        features.clone(),
    )]);
    arg
}

fn added_library(add: &AddLibrary) -> EffectiveLibrary {
    let path = add.name.relative_path();
    let source = match (&add.url, &add.repository) {
        (Some(url), _) => FileSource::Download {
            url: url.clone(),
            sha1: add.sha1,
            size: add.size,
        },
        (None, Some(repository)) => {
            let url = format!("{}/{path}", repository.trim_end_matches('/'));
            match add.sha1 {
                Some(sha1) => FileSource::Download {
                    url,
                    sha1: Some(sha1),
                    size: add.size,
                },
                None => FileSource::Sidecar { url },
            }
        }
        (None, None) => FileSource::Existing,
    };
    EffectiveLibrary::new(add.name.clone(), Some(LibraryFile::new(path, source)))
}

fn apply_library(draft: &mut Draft<'_>, spec: &LibrarySpec) {
    let position = spec.pattern.as_deref().and_then(|pattern| {
        draft
            .profile()
            .libraries
            .iter()
            .position(|library| glob_matches(pattern, &library.name.to_string()))
    });
    if spec.remove
        && let Some(pattern) = &spec.pattern
    {
        draft.remove_libraries(pattern);
    }
    if let Some(add) = &spec.add {
        let anchor = position.and_then(|index| {
            draft
                .profile()
                .libraries
                .get(index)
                .map(|library| library.name.to_string())
        });
        draft.add_library(added_library(add), anchor.as_deref());
    }
}

fn apply_natives(profile: &DataProfile, draft: &mut Draft<'_>) {
    for spec in &profile.file.natives {
        let source = match &spec.source {
            NativesSpecSource::Keyword(NativesKeyword::Artifact) => NativesSource::Artifact,
            NativesSpecSource::Keyword(NativesKeyword::None) => NativesSource::NotNeeded,
            NativesSpecSource::System { system } => NativesSource::System(system.clone()),
            NativesSpecSource::Maven { maven, classifier } => NativesSource::Maven {
                repository: maven.clone(),
                classifier: classifier.clone(),
            },
        };
        draft.handle_natives(&spec.pattern, &source);
    }
}

fn apply_java(draft: &mut Draft<'_>, spec: &JavaSpec) {
    if let Some(major) = spec.major {
        draft.set_java_major(major);
    }
    if let Some(source) = spec.source {
        draft.set_java_source(match source {
            JavaSourceSpec::Any => JavaSource::Any,
            JavaSourceSpec::SystemOnly => JavaSource::SystemOnly,
        });
    }
    if let Some(runtime) = &spec.runtime {
        draft.set_java_runtime(runtime.clone());
    }
    if let Some(matching) = spec.matching {
        draft.set_java_match(match matching {
            JavaMatchSpec::Exact => JavaMatch::Exact,
            JavaMatchSpec::AtLeast => JavaMatch::AtLeast,
        });
    }
    if let Some(arch) = &spec.arch
        && let Ok(name) = coblet_types::ArchName::new(arch)
    {
        draft.set_java_arch(Arch::from(name));
    }
}
