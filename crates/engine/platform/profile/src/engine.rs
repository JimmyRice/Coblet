use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use coblet_mojang::{Argument, Condition, Library, RuleContext, evaluate};
use coblet_types::{Arch, ExtensionId, Platform};

use crate::effective::explain;
use crate::{
    Change, ChangeSource, Draft, EffectiveLaunchProfile, EffectiveLibrary, FileSource, JavaMatch,
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

    let mut maven_files = Vec::new();
    for file in &profile.maven_files {
        if evaluate(&file.rules, &cx).is_allowed_by_default() {
            maven_files.extend(library_file(file));
        }
    }

    let java = java_requirement(pipeline, profile);
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
        maven_files,
        jar_mods: profile.jar_mods.clone(),
        agents: profile.agents.clone(),
        java: JavaRequirement {
            major: java.0,
            component: java.1,
            runtime_platform: None,
            source: JavaSource::Any,
            arch: platform.arch().clone(),
            matching: JavaMatch::Exact,
            compatible: profile.compatible_java_majors.clone(),
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

/// Picks the Java major version and Mojang runtime component: the version's
/// own, unless the components accept only other versions, in which case the
/// newest they accept, with no Mojang component to download.
fn java_requirement(
    pipeline: &mut Pipeline<'_>,
    profile: &ResolvedProfile,
) -> (u32, Option<String>) {
    let java = profile.java.as_ref();
    let major = java.map_or(DEFAULT_JAVA_MAJOR, |java| java.major_version);
    let accepted = &profile.compatible_java_majors;
    match accepted.iter().max() {
        Some(&newest) if !accepted.contains(&major) => {
            pipeline.record(
                PipelineStep::Validate,
                ChangeSource::Metadata,
                "java",
                format!("{major} -> {newest}, the newest version the components accept"),
            );
            (newest, None)
        }
        _ => (major, java.map(|java| java.component.clone())),
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
mod tests {
    use std::sync::Arc;

    use coblet_mojang::{Condition, RawVersionJson, TraitTable};
    use coblet_runtime::Extension;
    use coblet_types::{Arch, ExtensionId, OsFamily, OsName, Platform};

    use super::ProfileEngine;
    use crate::{
        Change, ChangeSource, Draft, EffectiveLaunchProfile, FileSource, JavaSource, Missing,
        Natives, PipelineStep, PlatformProfile, ProfileArg, ProfileError, ResolvedProfile, Target,
    };

    fn resolved(name: &str) -> ResolvedProfile {
        let path = format!(
            "{}/../../../../fixtures/mojang/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        ResolvedProfile::from_version(raw, traits).unwrap()
    }

    fn platform(os: OsFamily, arch: Arch, version: &str) -> Platform {
        Platform::new(os, arch).with_os_version(version)
    }

    fn mac_arm() -> Platform {
        platform(OsFamily::MacOs, Arch::Aarch64, "15.5")
    }

    fn resolve(name: &str, platform: &Platform) -> Result<EffectiveLaunchProfile, ProfileError> {
        ProfileEngine::builtin().resolve(&resolved(name), platform)
    }

    #[test]
    fn carries_component_files_and_java_choices() {
        let component: crate::Component = serde_json::from_str(
            r#"{
                "uid": "acme.loader", "version": "1",
                "compatibleJavaMajors": [17, 21],
                "mavenFiles": [
                    { "name": "acme:installer:1", "url": "https://maven.example/" },
                    { "name": "acme:windows-only:1", "rules": [{ "action": "allow", "os": { "name": "windows" } }] }
                ],
                "jarMods": [{ "name": "acme:jarmod:1" }]
            }"#,
        )
        .unwrap();
        let composed = resolved("1.12.2").compose([&component]).unwrap();
        let profile = ProfileEngine::builtin()
            .resolve(&composed, &mac_arm())
            .unwrap();
        assert_eq!(profile.maven_files.len(), 1);
        assert_eq!(
            profile.maven_files[0].path,
            "acme/installer/1/installer-1.jar"
        );
        assert!(matches!(
            &profile.maven_files[0].source,
            FileSource::Sidecar { url } if url == "https://maven.example/acme/installer/1/installer-1.jar"
        ));
        assert_eq!(profile.jar_mods.len(), 1);
        assert_eq!(profile.java.major, 21);
        assert_eq!(profile.java.component, None);
        assert!(profile.explain().contains("8 -> 21"));

        let plain = resolve("1.12.2", &mac_arm()).unwrap();
        assert_eq!(plain.java.major, 8);
        assert_eq!(plain.java.component.as_deref(), Some("jre-legacy"));
    }

    #[test]
    fn data_profiles_can_accept_newer_java() {
        let relaxed = crate::DataProfile::from_toml(
            r#"
            id = "acme:newer-java"
            display_name = "Newer Java"
            [match]
            os = ["macos"]
            [java]
            match = "at-least"
            "#,
            crate::ProfileLayer::ThirdParty,
        )
        .unwrap();
        let mut engine = ProfileEngine::builtin();
        engine.register(Arc::new(relaxed)).unwrap();
        let profile = engine.resolve(&resolved("1.20.1"), &mac_arm()).unwrap();
        let java = &profile.java;
        assert_eq!(java.matching, crate::JavaMatch::AtLeast);
        assert!(java.accepts(21, &Arch::Aarch64));
        assert!(java.accepts(17, &Arch::Aarch64));
        assert!(!java.accepts(11, &Arch::Aarch64));
        assert!(!java.accepts(21, &Arch::X86_64));

        let strict = resolve("1.20.1", &mac_arm()).unwrap();
        assert!(!strict.java.accepts(21, &Arch::Aarch64));
    }

    fn ids(profile: &EffectiveLaunchProfile) -> Vec<&str> {
        profile.profiles.iter().map(ExtensionId::as_str).collect()
    }

    fn jvm(profile: &EffectiveLaunchProfile) -> Vec<&str> {
        profile
            .jvm_args
            .iter()
            .map(|arg| arg.value.as_str())
            .collect()
    }

    fn classpath(profile: &EffectiveLaunchProfile) -> Vec<&str> {
        profile.classpath().map(|file| file.path.as_str()).collect()
    }

    #[test]
    fn macos_arm64_runs_modern_versions_natively() {
        let profile = resolve("1.20.1", &mac_arm()).unwrap();
        assert_eq!(
            ids(&profile),
            ["coblet:mojang-macos-arm64", "coblet:macos-lwjgl3"]
        );
        assert_eq!(profile.java.major, 17);
        assert_eq!(
            profile.java.runtime_platform.as_deref(),
            Some("mac-os-arm64")
        );
        assert_eq!(profile.java.arch, Arch::Aarch64);
        let paths = classpath(&profile);
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with("natives-macos-arm64.jar"))
        );
        assert!(
            !paths
                .iter()
                .any(|path| path.contains("natives-linux") || path.contains("natives-windows"))
        );
        let start = jvm(&profile)
            .iter()
            .filter(|arg| **arg == "-XstartOnFirstThread")
            .count();
        assert_eq!(start, 1);
        assert_eq!(profile.warnings, []);
        assert_eq!(profile.native_jars().count(), 0);
    }

    #[test]
    fn macos_arm64_runs_lwjgl2_under_rosetta() {
        for name in ["1.12.2", "b1.7.3"] {
            let profile = resolve(name, &mac_arm()).unwrap();
            assert_eq!(
                ids(&profile),
                ["coblet:macos-arm64-rosetta", "coblet:legacy-arguments"],
                "{name}"
            );
            assert_eq!(profile.java.major, 8);
            assert_eq!(profile.java.runtime_platform.as_deref(), Some("mac-os"));
            assert_eq!(profile.java.arch, Arch::X86_64);
            assert_eq!(profile.warnings.len(), 1);
            assert!(
                profile
                    .native_jars()
                    .all(|jar| jar.classifier == "natives-osx")
            );
            assert!(profile.native_jars().count() > 0);
            assert!(!jvm(&profile).contains(&"-XstartOnFirstThread"));
            assert_eq!(&jvm(&profile)[3..], ["-cp", "${classpath}"]);
            let demo = profile
                .game_args
                .iter()
                .find(|arg| arg.value == "--demo")
                .unwrap();
            assert!(matches!(demo.condition, Condition::Features(_)));
        }
        // b1.7.3 picks LWJGL 2.9.0, not the build reserved for Mac OS X 10.5.
        let beta = resolve("b1.7.3", &mac_arm()).unwrap();
        let jar = beta.native_jars().next().unwrap();
        assert!(jar.file.path.contains("/2.9.0/"), "{}", jar.file.path);
        assert_eq!(jar.exclude, ["META-INF/"]);
    }

    #[test]
    fn early_lwjgl3_also_uses_rosetta() {
        let profile = resolve("1.13.2", &mac_arm()).unwrap();
        assert_eq!(
            ids(&profile),
            ["coblet:macos-arm64-rosetta", "coblet:macos-lwjgl3"]
        );
        let classifiers: std::collections::BTreeSet<&str> = profile
            .native_jars()
            .map(|jar| jar.classifier.as_str())
            .collect();
        assert_eq!(classifiers, ["natives-macos", "natives-osx"].into());
        let start = jvm(&profile)
            .iter()
            .filter(|arg| **arg == "-XstartOnFirstThread")
            .count();
        assert_eq!(start, 1);
    }

    #[test]
    fn linux_and_windows_x64_use_mojang_data() {
        let linux = resolve("26.3", &platform(OsFamily::Linux, Arch::X86_64, "6.8")).unwrap();
        assert_eq!(ids(&linux), ["coblet:mojang-linux-x64"]);
        assert_eq!(linux.java.major, 25);
        assert_eq!(linux.java.runtime_platform.as_deref(), Some("linux"));
        assert!(
            classpath(&linux)
                .iter()
                .any(|path| path.ends_with("natives-linux.jar"))
        );
        assert!(!jvm(&linux).contains(&"-XstartOnFirstThread"));

        let windows = resolve(
            "1.12.2",
            &platform(OsFamily::Windows, Arch::X86_64, "10.0.26100"),
        )
        .unwrap();
        assert_eq!(
            windows.java.runtime_platform.as_deref(),
            Some("windows-x64")
        );
        assert!(
            windows
                .native_jars()
                .all(|jar| jar.classifier.starts_with("natives-windows"))
        );
        assert!(
            windows
                .native_jars()
                .all(|jar| !jar.classifier.contains("${arch}"))
        );
    }

    #[test]
    fn linux_arm64_replaces_lwjgl_natives() {
        let profile = resolve("1.20.1", &platform(OsFamily::Linux, Arch::Aarch64, "6.8")).unwrap();
        assert_eq!(ids(&profile), ["coblet:linux-arm64"]);
        let natives: Vec<_> = profile
            .libraries
            .iter()
            .filter(|library| library.natives != Natives::None)
            .collect();
        assert_eq!(natives.len(), 7);
        for library in natives {
            assert_eq!(library.name.classifier(), Some("natives-linux-arm64"));
            let file = library.classpath.as_ref().unwrap();
            let FileSource::Sidecar { url } = &file.source else {
                panic!("expected a sidecar source: {file:?}");
            };
            assert!(
                url.starts_with("https://repo1.maven.org/maven2/org/lwjgl/"),
                "{url}"
            );
        }
        assert_eq!(profile.java.runtime_platform, None);
        assert_eq!(profile.java.source, JavaSource::Any);

        // The Tracy natives are declared unnecessary instead of borrowed from x86_64.
        let modern = resolve("26.3", &platform(OsFamily::Linux, Arch::Aarch64, "6.8")).unwrap();
        assert!(
            !classpath(&modern)
                .iter()
                .any(|path| path.ends_with("natives-linux.jar"))
        );

        // LWJGL 2 has no arm64 build and nothing maps the platform: unsupported.
        let error =
            resolve("1.12.2", &platform(OsFamily::Linux, Arch::Aarch64, "6.8")).unwrap_err();
        assert!(
            error.missing().contains(&Missing::Natives {
                library: "org.lwjgl.lwjgl:lwjgl-platform:2.9.4-nightly-20150209".to_owned()
            }),
            "{error}"
        );
        assert!(
            error
                .missing()
                .iter()
                .any(|missing| matches!(missing, Missing::Java { major: 8, .. }))
        );
    }

    #[test]
    fn freebsd_sample_resolves_completely() {
        let freebsd = platform(OsFamily::FreeBsd, Arch::X86_64, "14.2");
        for name in ["1.20.1", "1.21.11", "26.3"] {
            let profile = resolve(name, &freebsd).unwrap();
            assert_eq!(ids(&profile), ["coblet:freebsd-system"], "{name}");
            assert_eq!(profile.rule_context.os_name, "linux");
            assert!(
                !classpath(&profile)
                    .iter()
                    .any(|path| path.contains("natives-")),
                "{name}: {:?}",
                classpath(&profile)
            );
            assert!(
                classpath(&profile)
                    .iter()
                    .any(|path| path.ends_with("lwjgl-3.3.1.jar") || path.contains("/lwjgl/3."))
            );
            assert_eq!(
                profile.native_paths,
                [std::path::PathBuf::from("/usr/local/lib")]
            );
            assert!(jvm(&profile).contains(&"-Dorg.lwjgl.librarypath=/usr/local/lib"));
            assert_eq!(profile.java.source, JavaSource::SystemOnly);
            assert!(profile.client.is_some());
            let explain = profile.explain();
            assert!(
                explain.starts_with(
                    "platform: freebsd x86_64 14.2\nprofiles: coblet:freebsd-system\n"
                )
            );
            assert!(
                explain.contains("[evaluate] coblet:freebsd-system: rules evaluate as os `linux`")
            );
            assert!(explain.contains("natives from system path `/usr/local/lib`"));
            assert!(explain.contains("[validate] metadata: result"));
        }
        // The sample only covers LWJGL 3.
        assert!(resolve("1.12.2", &freebsd).is_err());
    }

    #[test]
    fn unknown_platforms_are_unsupported() {
        let haiku = Platform::new(OsFamily::from(OsName::new("haiku").unwrap()), Arch::X86_64);
        let error = resolve("1.20.1", &haiku).unwrap_err();
        let ProfileError::Unsupported {
            missing, explain, ..
        } = &error
        else {
            panic!("{error}");
        };
        assert!(missing.contains(&Missing::Natives {
            library: "org.lwjgl:lwjgl-glfw:3.3.1".to_owned()
        }));
        assert!(
            missing
                .iter()
                .any(|m| matches!(m, Missing::Java { major: 17, .. }))
        );
        assert!(explain.contains("profiles: none"));
        assert!(
            error
                .to_string()
                .starts_with("haiku x86_64 is not supported: no native code for ")
        );
    }

    struct Meddler(ExtensionId);

    impl Extension for Meddler {
        fn id(&self) -> &ExtensionId {
            &self.0
        }
        fn display_name(&self) -> &'static str {
            "meddler"
        }
    }

    impl PlatformProfile for Meddler {
        fn matches(&self, _target: &Target<'_>) -> bool {
            true
        }
        fn rewrite(&self, draft: &mut Draft<'_>) -> Result<(), ProfileError> {
            draft.remove_game_args("--username")
        }
    }

    #[test]
    fn profiles_cannot_touch_authentication_arguments() {
        let mut engine = ProfileEngine::builtin();
        let meddler = Arc::new(Meddler(ExtensionId::new("acme:meddler").unwrap()));
        engine.register(meddler.clone()).unwrap();
        assert!(engine.register(meddler).is_err());
        let error = engine.resolve(&resolved("1.20.1"), &mac_arm()).unwrap_err();
        assert!(
            matches!(error, ProfileError::AuthArgument { .. }),
            "{error}"
        );
    }

    /// Code profile that edits the arguments through `Draft::profile_mut`.
    struct Direct(ExtensionId, fn(&mut EffectiveLaunchProfile));

    impl Extension for Direct {
        fn id(&self) -> &ExtensionId {
            &self.0
        }
        fn display_name(&self) -> &'static str {
            "direct"
        }
    }

    impl PlatformProfile for Direct {
        fn matches(&self, _target: &Target<'_>) -> bool {
            true
        }
        fn rewrite(&self, draft: &mut Draft<'_>) -> Result<(), ProfileError> {
            (self.1)(draft.profile_mut());
            Ok(())
        }
    }

    fn resolve_direct(
        edit: fn(&mut EffectiveLaunchProfile),
    ) -> Result<EffectiveLaunchProfile, ProfileError> {
        let mut engine = ProfileEngine::builtin();
        engine
            .register(Arc::new(Direct(
                ExtensionId::new("acme:direct").unwrap(),
                edit,
            )))
            .unwrap();
        engine.resolve(&resolved("1.20.1"), &mac_arm())
    }

    fn game(profile: &EffectiveLaunchProfile) -> Vec<&str> {
        profile
            .game_args
            .iter()
            .map(|arg| arg.value.as_str())
            .collect()
    }

    #[test]
    fn profile_mut_cannot_add_authentication_arguments() {
        let error = resolve_direct(|profile| {
            profile
                .jvm_args
                .push(ProfileArg::always("-Dleak=${auth_access_token}"));
        })
        .unwrap_err();
        assert!(
            matches!(
                &error,
                ProfileError::AuthArgument { profile, argument }
                    if profile.as_str() == "acme:direct"
                        && argument == "-Dleak=${auth_access_token}"
            ),
            "{error}"
        );
    }

    #[test]
    fn profile_mut_cannot_remove_authentication_arguments() {
        let error = resolve_direct(|profile| {
            profile
                .game_args
                .retain(|arg| arg.value != "${auth_access_token}");
        })
        .unwrap_err();
        assert!(
            matches!(
                &error,
                ProfileError::AuthArgument { profile, argument }
                    if profile.as_str() == "acme:direct" && argument == "${auth_access_token}"
            ),
            "{error}"
        );
    }

    #[test]
    fn profile_mut_may_reorder_and_change_other_arguments() {
        let before = resolve("1.20.1", &mac_arm()).unwrap();
        assert!(jvm(&before).contains(&"-cp"));
        let after = resolve_direct(|profile| {
            profile.game_args.reverse();
            profile
                .jvm_args
                .push(ProfileArg::always("-Dacme.tuned=true"));
            profile.jvm_args.retain(|arg| arg.value != "-cp");
        })
        .unwrap();
        let mut expected = game(&before);
        expected.reverse();
        assert_eq!(game(&after), expected);
        assert!(jvm(&after).contains(&"-Dacme.tuned=true"));
        assert!(!jvm(&after).contains(&"-cp"));
    }

    #[test]
    fn explain_lists_every_step() {
        let profile = resolve("1.12.2", &mac_arm()).unwrap();
        let explain = profile.explain();
        for needle in [
            "[raw] metadata: version 1.12.2",
            "[match] coblet:macos-arm64-rosetta: profile matched",
            "[evaluate] coblet:macos-arm64-rosetta: rules evaluate as os `osx`, arch `x86_64`, native",
            "excluded by rules",
            "[rewrite] coblet:legacy-arguments: jvm `-cp` appended",
            "[rewrite] coblet:macos-arm64-rosetta: java Mojang runtime platform set to `mac-os`",
        ] {
            assert!(explain.contains(needle), "missing {needle:?} in\n{explain}");
        }
    }

    #[test]
    fn explain_puts_customizer_changes_last() {
        let mut profile = resolve("1.12.2", &mac_arm()).unwrap();
        let id = ExtensionId::new("acme:proxy").unwrap();
        profile.changes.insert(
            0,
            Change::new(
                PipelineStep::Customize,
                ChangeSource::Customizer(id),
                "jvm `-Dhttp.proxyHost=127.0.0.1`",
                "appended",
            ),
        );
        let explain = profile.explain();
        let last = explain.lines().last().unwrap();
        assert_eq!(
            last,
            "[customize] acme:proxy: jvm `-Dhttp.proxyHost=127.0.0.1` appended"
        );
    }
}
