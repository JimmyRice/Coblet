use std::sync::Arc;

use coblet_mojang::{Condition, RawVersionJson, TraitTable};
use coblet_runtime::Extension;
use coblet_types::{Arch, ExtensionId, OsFamily, OsName, Platform};

use super::ProfileEngine;
use crate::{
    Change, ChangeSource, Draft, EffectiveLaunchProfile, FileSource, JavaSource, Missing, Natives,
    PipelineStep, PlatformProfile, ProfileArg, ProfileError, ResolvedProfile, Target,
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
    let error = resolve("1.12.2", &platform(OsFamily::Linux, Arch::Aarch64, "6.8")).unwrap_err();
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
            explain.starts_with("platform: freebsd x86_64 14.2\nprofiles: coblet:freebsd-system\n")
        );
        assert!(explain.contains("[evaluate] coblet:freebsd-system: rules evaluate as os `linux`"));
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
