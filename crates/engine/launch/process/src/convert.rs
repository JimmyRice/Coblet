use std::collections::BTreeSet;

use coblet_launch_command::{
    CommandProfile, EnvChange, FeatureClause, FeatureCondition, TemplateArg,
};
use coblet_mojang::{Condition, RuleAction};
use coblet_platform::{EffectiveLaunchProfile, EnvValue, ProfileArg};

use crate::GameLayout;

/// Turns an effective launch profile into the input of `coblet-launch-command`.
///
/// Library paths are resolved against `layout`, duplicates are dropped, the
/// client jar goes last on the classpath, arguments a platform rule excluded are dropped, and the
/// remaining feature rules become [`FeatureCondition`]s.
#[must_use]
pub fn command_profile(profile: &EffectiveLaunchProfile, layout: &GameLayout) -> CommandProfile {
    let mut command =
        CommandProfile::new(profile.game_version.as_str(), profile.main_class.clone());
    if let Some(version_type) = &profile.version_type {
        version_type.as_str().clone_into(&mut command.version_type);
    }
    // Some version JSONs list a library twice (once more for its natives).
    let mut seen = BTreeSet::new();
    command.classpath = profile
        .classpath()
        .filter(|file| seen.insert(file.path.as_str()))
        .map(|file| layout.library(&file.path))
        .collect();
    if profile.client.is_some() {
        command
            .classpath
            .push(layout.client_jar(&profile.game_version));
    }
    command.jvm_args = template_args(&profile.jvm_args);
    command.game_args = template_args(&profile.game_args);
    command.assets_index_name.clone_from(&profile.assets);
    command.env = profile
        .env
        .iter()
        .filter_map(|(name, value)| match value {
            EnvValue::Set(value) => Some(EnvChange::Set {
                name: name.clone(),
                value: value.clone(),
            }),
            EnvValue::PrependPaths(paths) => Some(EnvChange::PrependPaths {
                name: name.clone(),
                paths: paths.iter().map(Into::into).collect(),
            }),
            _ => None,
        })
        .collect();
    command
}

fn template_args(args: &[ProfileArg]) -> Vec<TemplateArg> {
    args.iter()
        .filter_map(|arg| {
            let condition = match &arg.condition {
                Condition::Always => FeatureCondition::Always,
                Condition::Never => return None,
                Condition::Features(clauses) => FeatureCondition::Clauses(
                    clauses
                        .iter()
                        .map(|clause| FeatureClause {
                            allow: clause.action == RuleAction::Allow,
                            features: clause.features.clone(),
                        })
                        .collect(),
                ),
            };
            Some(TemplateArg::when(arg.value.clone(), condition))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::path::Path;

    use coblet_launch_command::{LaunchAuth, LaunchCommand};
    use coblet_mojang::{AssetIndex, RawVersionJson, TraitTable};
    use coblet_platform::{EffectiveLaunchProfile, ProfileEngine, ProfileError, ResolvedProfile};
    use coblet_types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

    use super::command_profile;
    use crate::files::by_name_directory;
    use crate::steps::{OptionsInput, launch_options, logging_argument};
    use crate::{GameLayout, LaunchRequest};

    const FIXTURES: [&str; 9] = [
        "26.3",
        "1.21.11",
        "1.20.1",
        "1.13.2",
        "1.12.2",
        "1.5.2",
        "b1.7.3",
        "a1.2.6",
        "c0.30_01c",
    ];

    fn fixtures() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../fixtures/mojang")
    }

    fn resolved(name: &str) -> ResolvedProfile {
        let raw = RawVersionJson::from_slice(
            &std::fs::read(fixtures().join(format!("{name}.json"))).unwrap(),
        )
        .unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        ResolvedProfile::from_version(raw, traits).unwrap()
    }

    fn platforms() -> [(&'static str, Platform); 5] {
        [
            (
                "windows-x64",
                Platform::new(OsFamily::Windows, Arch::X86_64).with_os_version("10.0"),
            ),
            (
                "macos-arm64",
                Platform::new(OsFamily::MacOs, Arch::Aarch64).with_os_version("15.5"),
            ),
            (
                "linux-x64",
                Platform::new(OsFamily::Linux, Arch::X86_64).with_os_version("6.8"),
            ),
            (
                "linux-arm64",
                Platform::new(OsFamily::Linux, Arch::Aarch64).with_os_version("6.8"),
            ),
            (
                "freebsd-x64",
                Platform::new(OsFamily::FreeBsd, Arch::X86_64).with_os_version("14.2"),
            ),
        ]
    }

    fn game_assets(
        profile: &EffectiveLaunchProfile,
        layout: &GameLayout,
    ) -> Option<std::path::PathBuf> {
        let id = &profile.asset_index.as_ref()?.id;
        let bytes = std::fs::read(fixtures().join("assets").join(format!("{id}.json"))).ok()?;
        let index = AssetIndex::from_slice(&bytes).unwrap();
        by_name_directory(&index, layout, id)
    }

    fn command(profile: &EffectiveLaunchProfile, auth: LaunchAuth) -> LaunchCommand {
        let layout = GameLayout::new("/data", "/game");
        let mut request = LaunchRequest::new(auth);
        request.launcher = Some(("coblet".to_owned(), "test".to_owned()));
        let jvm_args: Vec<String> = logging_argument(profile, &layout).into_iter().collect();
        let java = Path::new("/java/bin/java");
        let assets = game_assets(profile, &layout);
        let options = launch_options(&OptionsInput {
            java,
            java_major: profile.java.major,
            os: profile.platform.os(),
            profile,
            layout: &layout,
            request: &request,
            game_assets: assets.as_deref(),
            natives_directory: None,
            jvm_args: &jvm_args,
        });
        LaunchCommand::from_profile(&command_profile(profile, &layout), &options).unwrap()
    }

    fn render(result: Result<EffectiveLaunchProfile, ProfileError>, auth: LaunchAuth) -> String {
        let profile = match result {
            Ok(profile) => profile,
            Err(error) => {
                let mut out = format!("error: {error}\n");
                for missing in error.missing() {
                    let _ = writeln!(out, "missing: {missing}");
                }
                return out;
            }
        };
        let mut out = String::new();
        let profiles: Vec<&str> = profile.profiles.iter().map(ExtensionId::as_str).collect();
        let _ = writeln!(out, "profiles: {}", profiles.join(", "));
        let java = &profile.java;
        let _ = writeln!(
            out,
            "java: {} {} runtime {}/{}",
            java.major,
            java.arch,
            java.runtime_platform.as_deref().unwrap_or("-"),
            java.component.as_deref().unwrap_or("-"),
        );
        for warning in &profile.warnings {
            let _ = writeln!(out, "warning: {}", warning.message);
        }
        for jar in profile.native_jars() {
            let _ = writeln!(out, "extract: {}", jar.file.path);
        }
        for (name, value) in &profile.env {
            let _ = writeln!(out, "env: {name} = {value:?}");
        }
        let command = command(&profile, auth);
        let _ = writeln!(out, "cwd: {}", command.cwd().display());
        let _ = writeln!(out, "{}", command.program().display());
        for arg in command.args() {
            let arg = arg.to_string_lossy();
            if arg.contains("/data/libraries/") || arg.contains("/data/versions/") {
                let separator = if arg.contains(';') { ';' } else { ':' };
                for entry in arg.split(separator) {
                    let _ = writeln!(out, "  {entry}{separator}");
                }
            } else {
                let _ = writeln!(out, "  {arg}");
            }
        }
        if let Some(file) = command.argfile() {
            let _ = writeln!(out, "argfile {}:\n{}", file.path.display(), file.content);
        }
        out
    }

    fn steve() -> PlayerName {
        PlayerName::new("Steve").unwrap()
    }

    #[test]
    fn launch_commands_per_fixture_and_platform() {
        let engine = ProfileEngine::builtin();
        for fixture in FIXTURES {
            let resolved = resolved(fixture);
            for (platform_name, platform) in platforms() {
                let text = render(
                    engine.resolve(&resolved, &platform),
                    LaunchAuth::offline(steve()),
                );
                insta::assert_snapshot!(format!("{fixture}__{platform_name}"), text);
            }
        }
    }

    #[test]
    fn demo_commands() {
        let engine = ProfileEngine::builtin();
        let mac = &platforms()[1].1;
        for fixture in ["1.20.1", "1.12.2"] {
            let text = render(
                engine.resolve(&resolved(fixture), mac),
                LaunchAuth::demo(steve()),
            );
            insta::assert_snapshot!(format!("{fixture}__macos-arm64__demo"), text);
        }
    }
}
