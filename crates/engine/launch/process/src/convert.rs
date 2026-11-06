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
#[path = "../tests/convert.rs"]
mod tests;
