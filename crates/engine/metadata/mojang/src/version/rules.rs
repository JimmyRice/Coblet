use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One entry of a Mojang `rules` list.
///
/// A list starts out disallowing; each rule whose conditions all hold sets
/// the outcome to its action, so the last matching rule wins. An empty list
/// allows.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// What happens when the conditions hold.
    pub action: RuleAction,
    /// Operating system condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<OsCondition>,
    /// Launcher feature condition, such as `{"is_demo_user": true}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features: BTreeMap<String, bool>,
}

impl Rule {
    /// Creates a rule without conditions.
    #[must_use]
    pub fn new(action: RuleAction) -> Self {
        Self {
            action,
            os: None,
            features: BTreeMap::new(),
        }
    }

    /// Adds an operating system condition.
    #[must_use]
    pub fn with_os(self, os: OsCondition) -> Self {
        Self {
            os: Some(os),
            ..self
        }
    }

    /// Adds a feature condition.
    #[must_use]
    pub fn with_feature(mut self, name: impl Into<String>, value: bool) -> Self {
        self.features.insert(name.into(), value);
        self
    }
}

/// Action of a [`Rule`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    /// Use the guarded item.
    Allow,
    /// Leave the guarded item out.
    Disallow,
}

/// Operating system condition of a [`Rule`].
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsCondition {
    /// Mojang OS name: `windows`, `osx` or `linux`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Architecture as the official launcher reports it; `x86` is the only value Mojang uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Regular expression matched against the OS version, such as `^10\.5\.\d$`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl OsCondition {
    /// Creates a condition on the OS name.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Self::default()
        }
    }
}

/// Values rules are evaluated against, supplied by Platform Profiles.
///
/// The OS name and architecture are the strings the official launcher would
/// report on the target machine, so a Profile for an unsupported system maps
/// it onto one Mojang knows (for example FreeBSD onto `linux`).
///
/// # Examples
///
/// ```
/// use coblet_mojang::{OsCondition, Rule, RuleAction, RuleContext, evaluate};
///
/// let cx = RuleContext::new("osx", "aarch64").with_os_version("15.5");
/// let rules = [
///     Rule::new(RuleAction::Allow),
///     Rule::new(RuleAction::Disallow).with_os(OsCondition::named("osx")),
/// ];
/// assert!(!evaluate(&rules, &cx).is_allowed_by_default());
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleContext {
    /// Mojang OS name.
    pub os_name: String,
    /// Architecture string matched against [`OsCondition::arch`].
    pub os_arch: String,
    /// OS version matched against [`OsCondition::version`]; conditions on the version fail when unknown.
    pub os_version: Option<String>,
}

impl RuleContext {
    /// Creates a context with an unknown OS version.
    #[must_use]
    pub fn new(os_name: impl Into<String>, os_arch: impl Into<String>) -> Self {
        Self {
            os_name: os_name.into(),
            os_arch: os_arch.into(),
            os_version: None,
        }
    }

    /// Sets the OS version.
    #[must_use]
    pub fn with_os_version(self, version: impl Into<String>) -> Self {
        Self {
            os_version: Some(version.into()),
            ..self
        }
    }

    /// Reports whether an OS condition holds.
    ///
    /// An invalid version pattern never matches.
    #[must_use]
    pub fn matches(&self, os: &OsCondition) -> bool {
        if os.name.as_ref().is_some_and(|name| *name != self.os_name) {
            return false;
        }
        if os.arch.as_ref().is_some_and(|arch| *arch != self.os_arch) {
            return false;
        }
        let Some(pattern) = &os.version else {
            return true;
        };
        let Some(version) = &self.os_version else {
            return false;
        };
        match regex_lite::Regex::new(pattern) {
            Ok(regex) => regex.is_match(version),
            Err(error) => {
                tracing::warn!(%pattern, %error, "ignoring rule with an invalid os version pattern");
                false
            }
        }
    }
}

/// Result of evaluating rules against a [`RuleContext`].
///
/// OS conditions are decided at once. Feature conditions depend on the
/// launch (demo mode, custom resolution, Quick Play), so they stay open
/// until [`Condition::allows`] is called with the launch's features.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Always used.
    Always,
    /// Never used on this platform.
    Never,
    /// Used depending on launch features; the last matching clause wins,
    /// starting from disallowed.
    Features(Vec<FeatureClause>),
}

/// Feature-only rule left after OS conditions were decided.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureClause {
    /// The action.
    pub action: RuleAction,
    /// Features that must have these values; empty always matches.
    pub features: BTreeMap<String, bool>,
}

impl Condition {
    /// Decides the condition for a set of launch features; missing features count as `false`.
    #[must_use]
    pub fn allows(&self, features: &BTreeMap<String, bool>) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Features(clauses) => {
                let mut allowed = false;
                for clause in clauses {
                    let holds = clause.features.iter().all(|(name, value)| {
                        features.get(name).copied().unwrap_or(false) == *value
                    });
                    if holds {
                        allowed = clause.action == RuleAction::Allow;
                    }
                }
                allowed
            }
        }
    }

    /// Decides the condition with every feature off.
    #[must_use]
    pub fn is_allowed_by_default(&self) -> bool {
        self.allows(&BTreeMap::new())
    }

    /// Returns the feature names the condition depends on.
    pub fn features(&self) -> impl Iterator<Item = &str> {
        let clauses: &[FeatureClause] = match self {
            Self::Features(clauses) => clauses,
            Self::Always | Self::Never => &[],
        };
        clauses
            .iter()
            .flat_map(|clause| clause.features.keys().map(String::as_str))
    }
}

/// Evaluates the OS conditions of a rule list and keeps the feature conditions open.
#[must_use]
pub fn evaluate(rules: &[Rule], cx: &RuleContext) -> Condition {
    if rules.is_empty() {
        return Condition::Always;
    }
    let mut clauses: Vec<FeatureClause> = Vec::new();
    for rule in rules {
        if rule.os.as_ref().is_some_and(|os| !cx.matches(os)) {
            continue;
        }
        if rule.features.is_empty() {
            // An unconditional match overrides everything before it.
            clauses.clear();
        }
        clauses.push(FeatureClause {
            action: rule.action,
            features: rule.features.clone(),
        });
    }
    match clauses.as_slice() {
        [] => Condition::Never,
        [only] if only.features.is_empty() => match only.action {
            RuleAction::Allow => Condition::Always,
            RuleAction::Disallow => Condition::Never,
        },
        _ => Condition::Features(clauses),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Condition, OsCondition, Rule, RuleAction, RuleContext, evaluate};

    fn os(name: &str, version: Option<&str>) -> OsCondition {
        OsCondition {
            name: Some(name.to_owned()),
            arch: None,
            version: version.map(str::to_owned),
        }
    }

    #[test]
    fn evaluates_lwjgl2_rules_by_os_version() {
        let rules = [
            Rule::new(RuleAction::Allow),
            Rule::new(RuleAction::Disallow).with_os(os("osx", Some(r"^10\.5\.\d$"))),
        ];
        let leopard = RuleContext::new("osx", "x86_64").with_os_version("10.5.8");
        let sequoia = RuleContext::new("osx", "x86_64").with_os_version("15.5");
        let unknown = RuleContext::new("osx", "x86_64");
        assert_eq!(evaluate(&rules, &leopard), Condition::Never);
        assert_eq!(evaluate(&rules, &sequoia), Condition::Always);
        assert_eq!(evaluate(&rules, &unknown), Condition::Always);
        assert_eq!(evaluate(&[], &unknown), Condition::Always);
        let only_osx = [Rule::new(RuleAction::Allow).with_os(OsCondition::named("osx"))];
        assert_eq!(
            evaluate(&only_osx, &RuleContext::new("linux", "x86_64")),
            Condition::Never
        );
    }

    #[test]
    fn matches_architecture_and_rejects_invalid_patterns() {
        let x86 = [Rule::new(RuleAction::Allow).with_os(OsCondition {
            arch: Some("x86".to_owned()),
            ..OsCondition::default()
        })];
        assert_eq!(
            evaluate(&x86, &RuleContext::new("windows", "x86")),
            Condition::Always
        );
        assert_eq!(
            evaluate(&x86, &RuleContext::new("windows", "x86_64")),
            Condition::Never
        );
        let broken = [Rule::new(RuleAction::Allow).with_os(os("osx", Some("(")))];
        let cx = RuleContext::new("osx", "aarch64").with_os_version("15.5");
        assert_eq!(evaluate(&broken, &cx), Condition::Never);
    }

    #[test]
    fn keeps_feature_conditions_open() {
        let demo = [Rule::new(RuleAction::Allow).with_feature("is_demo_user", true)];
        let condition = evaluate(&demo, &RuleContext::new("linux", "x86_64"));
        assert!(!condition.is_allowed_by_default());
        assert_eq!(condition.features().collect::<Vec<_>>(), ["is_demo_user"]);
        let on = BTreeMap::from([("is_demo_user".to_owned(), true)]);
        assert!(condition.allows(&on));

        // A later unconditional rule overrides earlier feature clauses.
        let overridden = [
            Rule::new(RuleAction::Allow).with_feature("is_demo_user", true),
            Rule::new(RuleAction::Disallow),
        ];
        assert_eq!(
            evaluate(&overridden, &RuleContext::new("linux", "x86_64")),
            Condition::Never
        );
        // Feature clauses after an unconditional allow can still disallow.
        let mixed = [
            Rule::new(RuleAction::Allow),
            Rule::new(RuleAction::Disallow).with_feature("has_custom_resolution", true),
        ];
        let condition = evaluate(&mixed, &RuleContext::new("linux", "x86_64"));
        assert!(condition.is_allowed_by_default());
        assert!(!condition.allows(&BTreeMap::from([(
            "has_custom_resolution".to_owned(),
            true
        )])));
    }
}
