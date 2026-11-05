//! Records what a launch customizer changed in the effective profile.

use std::collections::{BTreeMap, BTreeSet};

use coblet_platform::{
    Change, ChangeSource, EffectiveLaunchProfile, EffectiveLibrary, EnvValue, PipelineStep,
    ProfileArg, auth_argument_change,
};
use coblet_types::ExtensionId;

struct Recorder<'a> {
    id: &'a ExtensionId,
    changes: Vec<Change>,
}

impl Recorder<'_> {
    fn note(&mut self, subject: impl Into<String>, action: impl Into<String>) {
        self.changes.push(Change::new(
            PipelineStep::Customize,
            ChangeSource::Customizer(self.id.clone()),
            subject,
            action,
        ));
    }

    fn libraries(&mut self, before: &[EffectiveLibrary], after: &[EffectiveLibrary]) {
        let old: BTreeMap<String, &EffectiveLibrary> = before
            .iter()
            .map(|library| (library.native_key(), library))
            .collect();
        let new: BTreeMap<String, &EffectiveLibrary> = after
            .iter()
            .map(|library| (library.native_key(), library))
            .collect();
        let mut edited = false;
        for library in after {
            let key = library.native_key();
            match old.get(&key) {
                None => self.note(format!("library {key}"), "added"),
                Some(previous) if *previous != library => {
                    self.note(format!("library {key}"), "changed");
                }
                Some(_) => continue,
            }
            edited = true;
        }
        for library in before {
            let key = library.native_key();
            if !new.contains_key(&key) {
                self.note(format!("library {key}"), "removed");
                edited = true;
            }
        }
        if !edited && before != after {
            self.note("libraries", "reordered");
        }
    }

    fn args(&mut self, kind: &str, before: &[ProfileArg], after: &[ProfileArg]) {
        let mut left: Vec<&ProfileArg> = before.iter().collect();
        let mut edited = false;
        for arg in after {
            if let Some(index) = left.iter().position(|old| *old == arg) {
                left.remove(index);
            } else {
                self.note(kind, format!("`{}` added", arg.value));
                edited = true;
            }
        }
        for arg in left {
            self.note(kind, format!("`{}` removed", arg.value));
            edited = true;
        }
        if !edited && before != after {
            self.note(kind, "arguments reordered");
        }
    }

    fn env(&mut self, before: &BTreeMap<String, EnvValue>, after: &BTreeMap<String, EnvValue>) {
        for (name, value) in after {
            if before.get(name) != Some(value) {
                self.note(format!("env {name}"), describe_env(value));
            }
        }
        for name in before.keys() {
            if !after.contains_key(name) {
                self.note(format!("env {name}"), "removed");
            }
        }
    }

    fn native_paths(&mut self, before: &EffectiveLaunchProfile, after: &EffectiveLaunchProfile) {
        let old: BTreeSet<_> = before.native_paths.iter().collect();
        let new: BTreeSet<_> = after.native_paths.iter().collect();
        for path in new.difference(&old) {
            self.note("native path", format!("`{}` added", path.display()));
        }
        for path in old.difference(&new) {
            self.note("native path", format!("`{}` removed", path.display()));
        }
        if old == new && before.native_paths != after.native_paths {
            self.note("native paths", "reordered");
        }
    }

    fn java(&mut self, before: &EffectiveLaunchProfile, after: &EffectiveLaunchProfile) {
        let (old, new) = (&before.java, &after.java);
        if old.major != new.major {
            self.note("java", format!("major version set to {}", new.major));
        }
        if old.component != new.component {
            self.note(
                "java",
                format!(
                    "Mojang runtime component set to {}",
                    optional(new.component.as_deref())
                ),
            );
        }
        if old.runtime_platform != new.runtime_platform {
            self.note(
                "java",
                format!(
                    "Mojang runtime platform set to {}",
                    optional(new.runtime_platform.as_deref())
                ),
            );
        }
        if old.source != new.source {
            self.note("java", format!("source set to {:?}", new.source));
        }
        if old.arch != new.arch {
            self.note("java", format!("architecture set to {}", new.arch.as_str()));
        }
    }

    fn client(&mut self, before: &EffectiveLaunchProfile, after: &EffectiveLaunchProfile) {
        match (&before.client, &after.client) {
            (old, Some(new)) if old.as_ref() != Some(new) => {
                self.note("client jar", format!("set to `{}`", new.url));
            }
            (Some(_), None) => self.note("client jar", "removed"),
            _ => {}
        }
    }

    fn other_fields(&mut self, before: &EffectiveLaunchProfile, after: &EffectiveLaunchProfile) {
        let mut named = Vec::new();
        macro_rules! compare {
            ($($field:ident),*) => {
                $(if before.$field != after.$field {
                    named.push(stringify!($field));
                })*
            };
        }
        compare!(
            platform,
            game_version,
            version_type,
            release_time,
            asset_index,
            assets,
            logging,
            traits,
            rule_context,
            profiles,
            warnings
        );
        for field in &named {
            self.note(field.replace('_', " "), "changed");
        }
        let mut rest = after.clone();
        rest.main_class.clone_from(&before.main_class);
        rest.libraries.clone_from(&before.libraries);
        rest.client.clone_from(&before.client);
        rest.native_paths.clone_from(&before.native_paths);
        rest.jvm_args.clone_from(&before.jvm_args);
        rest.game_args.clone_from(&before.game_args);
        rest.env.clone_from(&before.env);
        rest.java.clone_from(&before.java);
        rest.platform.clone_from(&before.platform);
        rest.game_version.clone_from(&before.game_version);
        rest.version_type.clone_from(&before.version_type);
        rest.release_time.clone_from(&before.release_time);
        rest.asset_index.clone_from(&before.asset_index);
        rest.assets.clone_from(&before.assets);
        rest.logging.clone_from(&before.logging);
        rest.traits.clone_from(&before.traits);
        rest.rule_context.clone_from(&before.rule_context);
        rest.profiles.clone_from(&before.profiles);
        rest.warnings.clone_from(&before.warnings);
        rest.changes.clone_from(&before.changes);
        if rest != *before {
            self.note("profile", "other fields changed");
        }
    }
}

fn optional(value: Option<&str>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| format!("`{value}`"))
}

fn describe_env(value: &EnvValue) -> String {
    match value {
        EnvValue::Set(text) => format!("set to {text:?}"),
        EnvValue::PrependPaths(paths) => format!("prepends {}", paths.join(", ")),
        other => format!("set to {other:?}"),
    }
}

/// Returns the first authentication argument, JVM or game, that is in one
/// profile and not in the other; moving one is not a change.
pub(crate) fn auth_change(
    before: &EffectiveLaunchProfile,
    after: &EffectiveLaunchProfile,
) -> Option<String> {
    auth_argument_change(&before.jvm_args, &after.jvm_args)
        .or_else(|| auth_argument_change(&before.game_args, &after.game_args))
}

/// Appends a [`PipelineStep::Customize`] change to `after` for every
/// difference from `before`.
pub(crate) fn record(
    before: &EffectiveLaunchProfile,
    after: &mut EffectiveLaunchProfile,
    id: &ExtensionId,
) {
    let mut recorder = Recorder {
        id,
        changes: Vec::new(),
    };
    if before.main_class != after.main_class {
        recorder.note(
            "main class",
            format!(
                "set to `{}` (was `{}`)",
                after.main_class, before.main_class
            ),
        );
    }
    recorder.libraries(&before.libraries, &after.libraries);
    recorder.client(before, after);
    recorder.native_paths(before, after);
    recorder.args("jvm", &before.jvm_args, &after.jvm_args);
    recorder.args("game", &before.game_args, &after.game_args);
    recorder.env(&before.env, &after.env);
    recorder.java(before, after);
    recorder.other_fields(before, after);
    after.changes.extend(recorder.changes);
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
