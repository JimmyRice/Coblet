use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use coblet_mojang::{Argument, Condition, Library, RuleContext, evaluate};
use coblet_types::{Arch, ExtensionId, Platform};

use crate::effective::explain;
use crate::{
    Change, ChangeSource, Draft, EffectiveLaunchProfile, EffectiveLibrary, FileSource,
    JavaRequirement, JavaSource, LibraryFile, Missing, NativeJar, Natives, PipelineStep,
    PlatformProfile, ProfileArg, ProfileError, ProfileWarning, ResolvedProfile, RuleSettings,
    Target, auth_argument_change, is_auth_argument,
};

/// Default Maven repository of Mojang libraries.
pub const LIBRARIES_URL: &str = "https://libraries.minecraft.net/";

/// Java major version assumed for version files without `javaVersion`.
const DEFAULT_JAVA_MAJOR: u32 = 8;

/// Runs the Profile pipeline: Match, Evaluate, Rewrite and Validate.
///
/// Profiles apply in layer order ([`ProfileLayer`](crate::ProfileLayer)),
/// then by priority, then by ID, so the result is deterministic.
#[derive(Clone, Default)]
pub struct ProfileEngine {
    profiles: Vec<Arc<dyn PlatformProfile>>,
}

impl std::fmt::Debug for ProfileEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.profiles.iter().map(|profile| profile.id().as_str()))
            .finish()
    }
}

struct Pipeline<'a> {
    target: Target<'a>,
    matched: Vec<&'a Arc<dyn PlatformProfile>>,
    changes: Vec<Change>,
}

impl Pipeline<'_> {
    fn record(
        &mut self,
        step: PipelineStep,
        source: ChangeSource,
        subject: impl Into<String>,
        action: impl Into<String>,
    ) {
        self.changes.push(Change {
            step,
            source,
            subject: subject.into(),
            action: action.into(),
        });
    }
}

impl ProfileEngine {
    /// Creates an engine without profiles; every platform is unknown to it.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Creates an engine with the built-in profiles.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            profiles: crate::builtin::profiles(),
        }
    }

    /// Adds a profile.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::InvalidProfile`] when a profile with the same ID is registered.
    pub fn register(&mut self, profile: Arc<dyn PlatformProfile>) -> Result<(), ProfileError> {
        if self.profiles.iter().any(|known| known.id() == profile.id()) {
            return Err(ProfileError::InvalidProfile {
                name: profile.id().to_string(),
                message: "a profile with this id is already registered".to_owned(),
            });
        }
        self.profiles.push(profile);
        Ok(())
    }

    /// Returns the registered profiles.
    #[must_use]
    pub fn profiles(&self) -> &[Arc<dyn PlatformProfile>] {
        &self.profiles
    }

    /// Builds the launch profile of `profile` for `platform`.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Unsupported`] with what is missing when the
    /// platform cannot run the version, [`ProfileError::AuthArgument`]
    /// when a profile's rewrite adds or removes an authentication argument,
    /// by any means, and
    /// [`ProfileError::Failed`] when a matching profile fails in its rules
    /// or rewrite. A profile whose matching fails is skipped with a warning.
    pub fn resolve(
        &self,
        profile: &ResolvedProfile,
        platform: &Platform,
    ) -> Result<EffectiveLaunchProfile, ProfileError> {
        let target = Target { platform, profile };
        let mut matched: Vec<&Arc<dyn PlatformProfile>> = Vec::new();
        let mut skipped = Vec::new();
        for candidate in &self.profiles {
            match candidate.try_matches(&target) {
                Ok(true) => matched.push(candidate),
                Ok(false) => {}
                Err(error) => {
                    let reason = match error {
                        ProfileError::Failed { message, .. } => message,
                        other => other.to_string(),
                    };
                    skipped.push(ProfileWarning {
                        profile: candidate.id().clone(),
                        message: format!("skipped because matching failed: {reason}"),
                    });
                }
            }
        }
        matched.sort_by(|a, b| {
            (a.layer(), a.priority(), a.id()).cmp(&(b.layer(), b.priority(), b.id()))
        });
        let mut pipeline = Pipeline {
            target,
            matched,
            changes: Vec::new(),
        };
        pipeline.record(
            PipelineStep::Raw,
            ChangeSource::Metadata,
            format!("version {}", profile.game_version),
            format!(
                "main class `{}`, {} libraries, {} arguments",
                profile.main_class,
                profile.libraries.len(),
                if profile.legacy_arguments {
                    "legacy"
                } else {
                    "modern"
                }
            ),
        );
        for matched in pipeline.matched.clone() {
            pipeline.record(
                PipelineStep::Match,
                ChangeSource::Profile(matched.id().clone()),
                "profile",
                format!(
                    "matched ({:?}, priority {})",
                    matched.layer(),
                    matched.priority()
                ),
            );
        }
        for warning in &skipped {
            pipeline.record(
                PipelineStep::Match,
                ChangeSource::Profile(warning.profile.clone()),
                "warning",
                format!("`{}`", warning.message),
            );
        }

        let settings = rule_settings(&mut pipeline)?;
        let mut effective = evaluate_profile(&mut pipeline, &settings);
        effective.warnings.extend(skipped);

        let mut refusals = Vec::new();
        for matched in pipeline.matched.clone() {
            let jvm_args = auth_args(&effective.jvm_args);
            let game_args = auth_args(&effective.game_args);
            let mut draft = Draft {
                profile: &mut effective,
                source: matched.id().clone(),
                refusals: &mut refusals,
            };
            matched.rewrite(&mut draft)?;
            // `Draft::profile_mut` skips the checks of the other methods.
            if let Some(argument) = auth_argument_change(&jvm_args, &effective.jvm_args)
                .or_else(|| auth_argument_change(&game_args, &effective.game_args))
            {
                return Err(ProfileError::AuthArgument {
                    profile: matched.id().clone(),
                    argument,
                });
            }
        }

        validate(&settings, effective, refusals, native_libraries(profile))
    }
}

fn auth_args(args: &[ProfileArg]) -> Vec<ProfileArg> {
    args.iter()
        .filter(|arg| is_auth_argument(&arg.value))
        .cloned()
        .collect()
}

fn rule_settings(pipeline: &mut Pipeline<'_>) -> Result<RuleSettings, ProfileError> {
    let mut settings = RuleSettings::default();
    let target = pipeline.target;
    for profile in pipeline.matched.clone() {
        let before = settings.clone();
        profile.try_rules(&target, &mut settings)?;
        if settings != before {
            pipeline.record(
                PipelineStep::Evaluate,
                ChangeSource::Profile(profile.id().clone()),
                "rules",
                format!(
                    "evaluate as os `{}`, arch `{}`{}",
                    settings.os_name.as_deref().unwrap_or("?"),
                    settings
                        .os_arch
                        .as_deref()
                        .unwrap_or_else(|| target.platform.arch().as_str()),
                    if settings.native { ", native" } else { "" }
                ),
            );
        }
    }
    Ok(settings)
}

fn default_natives_arch(arch: &Arch) -> &'static str {
    match arch {
        Arch::X86 | Arch::Arm => "32",
        _ => "64",
    }
}

fn library_file(library: &Library) -> Option<LibraryFile> {
    let downloads = library.downloads.as_ref();
    if let Some(artifact) = downloads.and_then(|downloads| downloads.artifact.as_ref()) {
        let path = artifact
            .path
            .clone()
            .unwrap_or_else(|| library.name.relative_path());
        let source = if artifact.url.is_empty() {
            FileSource::Existing
        } else {
            FileSource::Download {
                url: artifact.url.clone(),
                sha1: artifact.sha1,
                size: artifact.size,
            }
        };
        return Some(LibraryFile::new(path, source));
    }
    if downloads.is_some_and(|downloads| !downloads.classifiers.is_empty()) {
        // A natives-only entry before 1.19: nothing goes on the classpath.
        return None;
    }
    let path = library.name.relative_path();
    let base = library.url.as_deref().unwrap_or(LIBRARIES_URL);
    Some(LibraryFile::new(
        path.clone(),
        FileSource::Sidecar {
            url: format!("{}/{path}", base.trim_end_matches('/')),
        },
    ))
}

fn native_jar(library: &Library, classifier: &str) -> NativeJar {
    let artifact = library
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.classifiers.get(classifier));
    let file = if let Some(artifact) = artifact {
        LibraryFile::new(
            artifact.path.clone().unwrap_or_else(|| {
                library
                    .name
                    .clone()
                    .with_classifier(classifier)
                    .map_or_else(
                        |_| library.name.relative_path(),
                        |name| name.relative_path(),
                    )
            }),
            FileSource::Download {
                url: artifact.url.clone(),
                sha1: artifact.sha1,
                size: artifact.size,
            },
        )
    } else {
        let path = library
            .name
            .clone()
            .with_classifier(classifier)
            .map_or_else(
                |_| library.name.relative_path(),
                |name| name.relative_path(),
            );
        let base = library.url.as_deref().unwrap_or(LIBRARIES_URL);
        LibraryFile::new(
            path.clone(),
            FileSource::Sidecar {
                url: format!("{}/{path}", base.trim_end_matches('/')),
            },
        )
    };
    NativeJar {
        file,
        classifier: classifier.to_owned(),
        exclude: library
            .extract
            .as_ref()
            .map(|extract| extract.exclude.clone())
            .unwrap_or_default(),
    }
}

fn has_native_classifier(library: &Library) -> bool {
    library
        .name
        .classifier()
        .is_some_and(|classifier| classifier.starts_with("natives-"))
}

/// Libraries that carry native code on some platform, by `group:artifact:version`.
fn native_libraries(profile: &ResolvedProfile) -> BTreeSet<String> {
    profile
        .libraries
        .iter()
        .filter(|library| has_native_classifier(library) || !library.natives.is_empty())
        .map(|library| {
            let name = &library.name;
            format!("{}:{}:{}", name.group(), name.artifact(), name.version())
        })
        .collect()
}

fn evaluate_args(args: &[Argument], cx: &RuleContext) -> Vec<ProfileArg> {
    let mut out = Vec::new();
    for argument in args {
        let (values, condition) = match argument {
            Argument::Plain(value) => (std::slice::from_ref(value), Condition::Always),
            Argument::Conditional { rules, value } => (value.as_slice(), evaluate(rules, cx)),
        };
        if condition == Condition::Never {
            continue;
        }
        out.extend(values.iter().map(|value| ProfileArg {
            value: value.clone(),
            condition: condition.clone(),
        }));
    }
    out
}

fn evaluate_profile(
    pipeline: &mut Pipeline<'_>,
    settings: &RuleSettings,
) -> EffectiveLaunchProfile {
    let Target { platform, profile } = pipeline.target;
    let mut cx = RuleContext::new(
        settings
            .os_name
            .clone()
            .unwrap_or_else(|| platform.os().as_str().to_owned()),
        settings
            .os_arch
            .clone()
            .unwrap_or_else(|| platform.arch().as_str().to_owned()),
    );
    if let Some(version) = settings
        .os_version
        .as_deref()
        .or_else(|| platform.os_version())
    {
        cx = cx.with_os_version(version);
    }
    let natives_arch = settings
        .natives_arch
        .clone()
        .unwrap_or_else(|| default_natives_arch(platform.arch()).to_owned());

    let mut libraries = Vec::new();
    for library in &profile.libraries {
        let condition = evaluate(&library.rules, &cx);
        let name = library.name.to_string();
        if !condition.is_allowed_by_default() {
            pipeline.record(
                PipelineStep::Evaluate,
                ChangeSource::Metadata,
                format!("library {name}"),
                "excluded by rules",
            );
            continue;
        }
        let mut effective = EffectiveLibrary::new(library.name.clone(), library_file(library));
        if has_native_classifier(library) {
            effective.natives = Natives::Mojang;
        }
        if let Some(classifier) = library.natives.get(&cx.os_name) {
            let classifier = classifier.replace("${arch}", &natives_arch);
            effective.extract = Some(native_jar(library, &classifier));
            effective.natives = Natives::Mojang;
        }
        if effective.is_active() {
            libraries.push(effective);
        } else {
            pipeline.record(
                PipelineStep::Evaluate,
                ChangeSource::Metadata,
                format!("library {name}"),
                "has no file for this platform",
            );
        }
    }

    let java = profile.java.as_ref();
    EffectiveLaunchProfile {
        platform: platform.clone(),
        game_version: profile.game_version.clone(),
        version_type: profile.version_type.clone(),
        release_time: profile.release_time,
        main_class: profile.main_class.clone(),
        libraries,
        client: profile.client.clone(),
        native_paths: Vec::new(),
        jvm_args: evaluate_args(&profile.jvm_args, &cx),
        game_args: evaluate_args(&profile.game_args, &cx),
        env: BTreeMap::new(),
        java: JavaRequirement {
            major: java.map_or(DEFAULT_JAVA_MAJOR, |java| java.major_version),
            component: java.map(|java| java.component.clone()),
            runtime_platform: None,
            source: JavaSource::Any,
            arch: platform.arch().clone(),
        },
        asset_index: profile.asset_index.clone(),
        assets: profile.assets.clone(),
        logging: profile.logging.clone(),
        traits: profile.traits.clone(),
        rule_context: cx,
        profiles: pipeline
            .matched
            .iter()
            .map(|profile| profile.id().clone())
            .collect(),
        warnings: Vec::new(),
        changes: std::mem::take(&mut pipeline.changes),
    }
}

fn validate(
    settings: &RuleSettings,
    mut effective: EffectiveLaunchProfile,
    refusals: Vec<(ExtensionId, String)>,
    native_libraries: BTreeSet<String>,
) -> Result<EffectiveLaunchProfile, ProfileError> {
    let mut missing: Vec<Missing> = refusals
        .into_iter()
        .map(|(profile, reason)| Missing::Refused { profile, reason })
        .collect();

    if settings.os_name.is_none() {
        // No profile maps this platform onto Mojang's data: nothing is guessed.
        missing.extend(
            native_libraries
                .into_iter()
                .map(|library| Missing::Natives { library }),
        );
    } else if !settings.native {
        let mut unhandled = BTreeSet::new();
        for library in &effective.libraries {
            if library.natives == Natives::Mojang {
                unhandled.insert(library.native_key());
            }
        }
        missing.extend(
            unhandled
                .into_iter()
                .map(|library| Missing::Natives { library }),
        );
    }

    let java = &effective.java;
    if java.source == JavaSource::Any
        && java.runtime_platform.is_none()
        && settings.os_name.is_none()
    {
        missing.push(Missing::Java {
            major: java.major,
            reason: "no Mojang runtime for this platform and no profile allows a system Java"
                .to_owned(),
        });
    }

    if missing.is_empty() {
        effective.changes.push(Change {
            step: PipelineStep::Validate,
            source: ChangeSource::Metadata,
            subject: "result".to_owned(),
            action: format!(
                "{} libraries, {} native jars, java {}",
                effective.classpath().count(),
                effective.native_jars().count(),
                effective.java.major
            ),
        });
        return Ok(effective);
    }
    Err(ProfileError::Unsupported {
        explain: explain(&effective.platform, &effective.profiles, &effective.changes),
        platform: Box::new(effective.platform),
        missing,
    })
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
