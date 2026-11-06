use std::collections::BTreeMap;
use std::path::PathBuf;

/// Feature-only rule clause, as left after the platform rules were decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureClause {
    /// Whether a matching clause allows the argument.
    pub allow: bool,
    /// Features that must have these values; empty always matches.
    pub features: BTreeMap<String, bool>,
}

/// Launch features an argument depends on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FeatureCondition {
    /// Always passed.
    #[default]
    Always,
    /// Passed when the last matching clause allows it, starting from not passed.
    Clauses(Vec<FeatureClause>),
}

impl FeatureCondition {
    /// Creates a condition that holds when every listed feature has its value.
    #[must_use]
    pub fn when(features: impl IntoIterator<Item = (String, bool)>) -> Self {
        Self::Clauses(vec![FeatureClause {
            allow: true,
            features: features.into_iter().collect(),
        }])
    }

    /// Decides the condition; missing features count as `false`.
    #[must_use]
    pub fn holds(&self, features: &BTreeMap<String, bool>) -> bool {
        match self {
            Self::Always => true,
            Self::Clauses(clauses) => clauses.iter().fold(false, |allowed, clause| {
                let matches = clause
                    .features
                    .iter()
                    .all(|(name, value)| features.get(name).copied().unwrap_or(false) == *value);
                if matches { clause.allow } else { allowed }
            }),
        }
    }
}

/// Argument template such as `--gameDir` or `${game_directory}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateArg {
    /// The template.
    pub value: String,
    /// Features it depends on.
    pub condition: FeatureCondition,
}

impl TemplateArg {
    /// Creates an argument that is always passed.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            condition: FeatureCondition::Always,
        }
    }

    /// Creates an argument passed only when the condition holds.
    #[must_use]
    pub fn when(value: impl Into<String>, condition: FeatureCondition) -> Self {
        Self {
            value: value.into(),
            condition,
        }
    }
}

impl From<&str> for TemplateArg {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Change to the environment of the game process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvChange {
    /// Set a variable.
    Set {
        /// Variable name.
        name: String,
        /// Value.
        value: String,
    },
    /// Put directories in front of a search path variable such as `LD_LIBRARY_PATH`;
    /// the launcher keeps the inherited value after them.
    PrependPaths {
        /// Variable name.
        name: String,
        /// Directories.
        paths: Vec<PathBuf>,
    },
}

/// Everything about a version the command needs, already adapted to the platform.
///
/// `coblet-launch` builds it from an effective launch profile; minimal
/// launchers can fill it in by hand.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandProfile {
    /// `${version_name}`.
    pub version_name: String,
    /// `${version_type}`.
    pub version_type: String,
    /// Java class with the `main` method.
    pub main_class: String,
    /// Classpath entries in order, including the client jar.
    pub classpath: Vec<PathBuf>,
    /// JVM argument templates.
    pub jvm_args: Vec<TemplateArg>,
    /// Game argument templates.
    pub game_args: Vec<TemplateArg>,
    /// `${assets_index_name}`.
    pub assets_index_name: String,
    /// Environment changes.
    pub env: Vec<EnvChange>,
}

impl CommandProfile {
    /// Creates a profile with a version name and main class.
    #[must_use]
    pub fn new(version_name: impl Into<String>, main_class: impl Into<String>) -> Self {
        Self {
            version_name: version_name.into(),
            main_class: main_class.into(),
            version_type: "release".to_owned(),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{FeatureClause, FeatureCondition};

    #[test]
    fn decides_feature_conditions() {
        let demo = FeatureCondition::when([("is_demo_user".to_owned(), true)]);
        assert!(!demo.holds(&BTreeMap::new()));
        assert!(demo.holds(&BTreeMap::from([("is_demo_user".to_owned(), true)])));
        assert!(FeatureCondition::Always.holds(&BTreeMap::new()));
        let not_custom = FeatureCondition::Clauses(vec![
            FeatureClause {
                allow: true,
                features: BTreeMap::new(),
            },
            FeatureClause {
                allow: false,
                features: BTreeMap::from([("has_custom_resolution".to_owned(), true)]),
            },
        ]);
        assert!(not_custom.holds(&BTreeMap::new()));
        assert!(!not_custom.holds(&BTreeMap::from([(
            "has_custom_resolution".to_owned(),
            true
        )])));
    }
}
