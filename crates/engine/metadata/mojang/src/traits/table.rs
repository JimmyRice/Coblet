use std::collections::HashSet;
use std::sync::LazyLock;

use coblet_catalog::Timestamp;
use coblet_runtime::{BoxFuture, Extension};
use coblet_types::{ExtensionId, GameVersionId, VersionType};
use serde::Deserialize;

use super::{
    TraitContext, TraitError, TraitInput, TraitSource, TraitTarget, VersionTrait, VersionTraits,
};
use crate::RawVersionJson;

/// Main class of Mojang's launchwrapper.
pub const LAUNCHWRAPPER_MAIN: &str = "net.minecraft.launchwrapper.Launch";

/// Text of the built-in trait table, `data/version-traits.toml`.
///
/// Copy it to start a table that replaces the built-in one under the same ID.
pub const BUILTIN_TRAITS_TOML: &str = include_str!("../../data/version-traits.toml");

/// Trait source described as data: rules that add or remove traits when
/// their conditions hold.
///
/// Tables are written in TOML or JSON with the same fields:
///
/// - `id` (required) and `name`;
/// - `[[rule]]` entries, run in file order. A rule adds the traits in
///   `add` and removes those in `remove` when every condition it sets
///   holds; a rule without conditions applies to every version.
///
/// Conditions known from a version list: `ids`, `types` (release
/// channels such as `old_beta`), `sources` (IDs of the version sources
/// that listed the version), `released_before` and `released_after`
/// (RFC 3339, exclusive). Conditions that need the version JSON, so the
/// rule is skipped while a list is built and applies when a version is
/// launched: `assets` (asset index IDs), `main_class`, `tweakers` (any
/// launchwrapper tweaker passed with `--tweakClass`) and `java_component`
/// (`javaVersion.component`). A rule may also carry a `note`.
///
/// The built-in table, ID `coblet:version-traits`, is
/// [`BUILTIN_TRAITS_TOML`].
///
/// # Examples
///
/// ```
/// use coblet_mojang::{TraitTable, VersionTrait};
/// use coblet_runtime::Extension;
/// use coblet_types::GameVersionId;
///
/// let table = TraitTable::from_toml(r#"
///     id = "acme:fixes"
///     name = "Acme fixes"
///
///     [[rule]]
///     add = ["acme:offline-skins"]
///     released_before = "2014-04-09T07:52:06Z"
///
///     [[rule]]
///     remove = ["acme:offline-skins"]
///     ids = ["b1.7.3"]
/// "#)?;
/// assert_eq!(table.id().as_str(), "acme:fixes");
/// let released = "2011-07-07T22:00:00Z".parse().ok();
/// let old = table.for_manifest_entry(&GameVersionId::new("a1.2.6")?, released);
/// assert!(old.contains(&VersionTrait::from("acme:offline-skins")));
/// assert!(table.for_manifest_entry(&GameVersionId::new("b1.7.3")?, released).is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitTable {
    id: ExtensionId,
    name: String,
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "rule")]
    rules: Vec<RuleDefinition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDefinition {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    released_before: Option<Timestamp>,
    #[serde(default)]
    released_after: Option<Timestamp>,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    main_class: Vec<String>,
    #[serde(default)]
    tweakers: Vec<String>,
    #[serde(default)]
    java_component: Vec<String>,
    #[serde(default)]
    #[allow(dead_code, reason = "documents the rule in the data file")]
    note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rule {
    add: Vec<VersionTrait>,
    remove: Vec<VersionTrait>,
    ids: HashSet<String>,
    types: Vec<VersionType>,
    sources: Vec<ExtensionId>,
    released_before: Option<Timestamp>,
    released_after: Option<Timestamp>,
    assets: HashSet<String>,
    main_class: HashSet<String>,
    tweakers: HashSet<String>,
    java_component: HashSet<String>,
}

static BUILTIN: LazyLock<TraitTable> = LazyLock::new(|| {
    TraitTable::from_toml(BUILTIN_TRAITS_TOML).unwrap_or_else(|error| {
        tracing::error!(%error, "the built-in version trait table is invalid");
        TraitTable {
            id: ExtensionId::new("coblet:version-traits").unwrap_or_else(|_| unreachable!()),
            name: "Built-in version traits".to_owned(),
            rules: Vec::new(),
        }
    })
});

/// What a rule is checked against.
struct Facts<'a> {
    id: &'a GameVersionId,
    version_type: Option<&'a VersionType>,
    released: Option<Timestamp>,
    source: Option<&'a ExtensionId>,
    raw: Option<&'a RawVersionJson>,
}

impl<'a> Facts<'a> {
    fn of_input(input: &'a TraitInput) -> Self {
        Self {
            id: &input.id,
            version_type: input.version_type.as_ref(),
            released: input.released,
            source: input.source.as_ref(),
            raw: input.version_json(),
        }
    }
}

impl Rule {
    fn from_definition(index: usize, rule: RuleDefinition) -> Result<Self, TraitError> {
        let invalid = |reason: String| TraitError::Table(format!("rule {}: {reason}", index + 1));
        let names = |values: Vec<String>, field: &str| {
            values
                .into_iter()
                .map(|name| {
                    if name.is_empty() || name.chars().any(char::is_whitespace) {
                        Err(invalid(format!("{field}: invalid trait name `{name}`")))
                    } else {
                        Ok(VersionTrait::from(name))
                    }
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let add = names(rule.add, "add")?;
        let remove = names(rule.remove, "remove")?;
        if add.is_empty() && remove.is_empty() {
            return Err(invalid("sets neither `add` nor `remove`".to_owned()));
        }
        if let Some(both) = add.iter().find(|value| remove.contains(value)) {
            return Err(invalid(format!("both adds and removes `{both}`")));
        }
        let sources = rule
            .sources
            .iter()
            .map(|id| {
                ExtensionId::new(id).map_err(|error| invalid(format!("sources: `{id}`: {error}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            add,
            remove,
            ids: rule.ids.into_iter().collect(),
            types: rule.types.into_iter().map(VersionType::from).collect(),
            sources,
            released_before: rule.released_before,
            released_after: rule.released_after,
            assets: rule.assets.into_iter().collect(),
            main_class: rule.main_class.into_iter().collect(),
            tweakers: rule.tweakers.into_iter().collect(),
            java_component: rule.java_component.into_iter().collect(),
        })
    }

    fn needs_version_json(&self) -> bool {
        !(self.assets.is_empty()
            && self.main_class.is_empty()
            && self.tweakers.is_empty()
            && self.java_component.is_empty())
    }

    fn applies(&self, facts: &Facts<'_>) -> bool {
        if !self.ids.is_empty() && !self.ids.contains(facts.id.as_str()) {
            return false;
        }
        if !self.types.is_empty()
            && !facts
                .version_type
                .is_some_and(|kind| self.types.contains(kind))
        {
            return false;
        }
        if !self.sources.is_empty()
            && !facts
                .source
                .is_some_and(|source| self.sources.contains(source))
        {
            return false;
        }
        if self.released_before.is_some() || self.released_after.is_some() {
            let Some(released) = facts.released else {
                return false;
            };
            if self
                .released_before
                .is_some_and(|before| released >= before)
                || self.released_after.is_some_and(|after| released <= after)
            {
                return false;
            }
        }
        if !self.needs_version_json() {
            return true;
        }
        let Some(raw) = facts.raw else {
            return false;
        };
        let assets = raw
            .asset_index
            .as_ref()
            .map(|index| index.id.as_str())
            .or(raw.assets.as_deref());
        (self.assets.is_empty() || assets.is_some_and(|assets| self.assets.contains(assets)))
            && (self.main_class.is_empty()
                || raw
                    .main_class
                    .as_deref()
                    .is_some_and(|main| self.main_class.contains(main)))
            && (self.tweakers.is_empty()
                || raw
                    .tweak_classes()
                    .iter()
                    .any(|tweaker| self.tweakers.contains(*tweaker)))
            && (self.java_component.is_empty()
                || raw
                    .java_version
                    .as_ref()
                    .is_some_and(|java| self.java_component.contains(&java.component)))
    }
}

impl TraitTable {
    /// Returns the built-in table, ID `coblet:version-traits`.
    #[must_use]
    pub fn builtin() -> &'static Self {
        &BUILTIN
    }

    /// Parses a table in TOML.
    ///
    /// # Errors
    ///
    /// Returns [`TraitError::Table`] when the text is malformed, has an
    /// unknown field or a rule that changes nothing.
    pub fn from_toml(text: &str) -> Result<Self, TraitError> {
        let definition: Definition =
            toml::from_str(text).map_err(|error| TraitError::Table(error.to_string()))?;
        Self::from_definition(definition)
    }

    /// Parses a table in JSON, with the same fields as the TOML form.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`from_toml`](Self::from_toml).
    pub fn from_json(text: &str) -> Result<Self, TraitError> {
        let definition: Definition =
            serde_json::from_str(text).map_err(|error| TraitError::Table(error.to_string()))?;
        Self::from_definition(definition)
    }

    fn from_definition(definition: Definition) -> Result<Self, TraitError> {
        let id = ExtensionId::new(&definition.id)
            .map_err(|error| TraitError::Table(format!("id `{}`: {error}", definition.id)))?;
        let rules = definition
            .rules
            .into_iter()
            .enumerate()
            .map(|(index, rule)| Rule::from_definition(index, rule))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            name: definition.name.unwrap_or_else(|| definition.id.clone()),
            id,
            rules,
        })
    }

    /// Changes `traits` by the rules that apply to `input`, in order.
    pub fn apply(&self, input: &TraitInput, traits: &mut VersionTraits) {
        self.apply_facts(&Facts::of_input(input), traits);
    }

    /// Returns the traits the table gives a version known from a version
    /// list alone: rules that need the version JSON are skipped.
    #[must_use]
    pub fn for_manifest_entry(
        &self,
        id: &GameVersionId,
        released: Option<Timestamp>,
    ) -> VersionTraits {
        let mut traits = VersionTraits::new();
        self.apply_facts(
            &Facts {
                id,
                version_type: None,
                released,
                source: None,
                raw: None,
            },
            &mut traits,
        );
        traits
    }

    /// Returns the traits the table gives a version JSON, starting from none.
    #[must_use]
    pub fn for_version(&self, raw: &RawVersionJson) -> VersionTraits {
        let mut traits = VersionTraits::new();
        self.apply_facts(
            &Facts {
                id: &raw.id,
                version_type: raw.version_type.as_ref(),
                released: raw.release_time,
                source: None,
                raw: Some(raw),
            },
            &mut traits,
        );
        traits
    }

    fn apply_facts(&self, facts: &Facts<'_>, traits: &mut VersionTraits) {
        for rule in self.rules.iter().filter(|rule| rule.applies(facts)) {
            traits.extend(rule.add.iter().cloned());
            for value in &rule.remove {
                traits.remove(value);
            }
        }
    }
}

impl Extension for TraitTable {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl TraitSource for TraitTable {
    fn adjust<'a>(
        &'a self,
        _cx: &'a TraitContext<'a>,
        versions: &'a mut [TraitTarget],
    ) -> BoxFuture<'a, Result<(), TraitError>> {
        Box::pin(async move {
            for target in versions.iter_mut() {
                let facts = Facts::of_input(target.input());
                let mut traits = target.traits().clone();
                self.apply_facts(&facts, &mut traits);
                *target.traits_mut() = traits;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use coblet_types::{ExtensionId, GameVersionId, VersionType};

    use super::{BUILTIN_TRAITS_TOML, TraitTable, VersionTrait, VersionTraits};
    use crate::{Document, RawVersionJson, TraitInput};

    fn fixture(name: &str) -> RawVersionJson {
        let path = format!(
            "{}/../../../../fixtures/mojang/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn names(traits: &VersionTraits) -> Vec<&str> {
        traits.iter().map(VersionTrait::as_str).collect()
    }

    #[test]
    fn builtin_table_parses() {
        let table = TraitTable::from_toml(BUILTIN_TRAITS_TOML).unwrap();
        assert_eq!(&table, TraitTable::builtin());
        assert_eq!(table.id.as_str(), "coblet:version-traits");
        assert_eq!(table.rules.len(), 4);
    }

    #[test]
    fn derives_traits_from_fixtures() {
        let table = TraitTable::builtin();
        assert_eq!(
            names(&table.for_version(&fixture("c0.30_01c"))),
            [
                "applet",
                "jre-legacy",
                "legacy-online-services",
                "legacy-macos-rendering"
            ]
        );
        assert_eq!(
            names(&table.for_version(&fixture("b1.7.3"))),
            [
                "jre-legacy",
                "legacy-online-services",
                "legacy-macos-rendering"
            ]
        );
        assert_eq!(
            names(&table.for_version(&fixture("1.5.2"))),
            [
                "jre-legacy",
                "legacy-online-services",
                "legacy-macos-rendering"
            ]
        );
        assert_eq!(
            names(&table.for_version(&fixture("1.12.2"))),
            ["jre-legacy"]
        );
        assert!(table.for_version(&fixture("1.20.1")).is_empty());
        assert!(table.for_version(&fixture("26.3")).is_empty());
    }

    #[test]
    fn manifest_entries_skip_rules_that_need_the_version_json() {
        let table = TraitTable::builtin();
        let id = GameVersionId::new("b1.7.3").unwrap();
        let released = "2011-07-07T22:00:00+00:00".parse().ok();
        assert_eq!(
            names(&table.for_manifest_entry(&id, released)),
            ["legacy-online-services"]
        );
        assert!(table.for_manifest_entry(&id, None).is_empty());
    }

    #[test]
    fn rules_run_in_order_and_match_list_facts() {
        let table = TraitTable::from_toml(
            r#"
            id = "test:table"

            [[rule]]
            add = ["x", "y"]
            types = ["experiment"]

            [[rule]]
            remove = ["y"]
            sources = ["acme:experiments"]

            [[rule]]
            add = ["z"]
            "#,
        )
        .unwrap();
        assert_eq!(table.name, "test:table");
        let input = |kind: &str, source: Option<&str>| {
            let mut input = TraitInput::new(GameVersionId::new("combat-8c").unwrap());
            input.version_type = Some(VersionType::from(kind));
            input.source = source.map(|id| ExtensionId::new(id).unwrap());
            input
        };
        let run = |input: &TraitInput, start: &[&str]| {
            let mut traits: VersionTraits =
                start.iter().map(|name| VersionTrait::from(*name)).collect();
            table.apply(input, &mut traits);
            traits
        };
        assert_eq!(
            names(&run(&input("experiment", None), &[])),
            ["x", "y", "z"]
        );
        assert_eq!(
            names(&run(&input("experiment", Some("acme:experiments")), &[])),
            ["x", "z"]
        );
        assert_eq!(
            names(&run(
                &input("release", Some("acme:experiments")),
                &["y", "w"]
            )),
            ["w", "z"]
        );
    }

    #[test]
    fn version_json_rules_apply_once_the_document_is_known() {
        let table = TraitTable::from_json(
            r#"{"id": "test:json", "rule": [
                {"add": ["acme:launchwrapper"], "main_class": ["net.minecraft.launchwrapper.Launch"]},
                {"remove": ["jre-legacy"], "java_component": ["jre-legacy"], "ids": ["c0.30_01c"]}
            ]}"#,
        )
        .unwrap();
        let raw = fixture("c0.30_01c");
        let mut listed = TraitInput::new(raw.id.clone());
        let mut traits: VersionTraits = [VersionTrait::JreLegacy].into_iter().collect();
        table.apply(&listed, &mut traits);
        assert_eq!(names(&traits), ["jre-legacy"]);
        listed = listed.with_version(Arc::new(Document {
            value: raw,
            bytes: Bytes::new(),
        }));
        assert_eq!(
            listed.version_type.as_ref().map(VersionType::as_str),
            Some("old_alpha")
        );
        table.apply(&listed, &mut traits);
        assert_eq!(names(&traits), ["acme:launchwrapper"]);
    }

    #[test]
    fn rejects_malformed_tables() {
        let error = |text: &str| TraitTable::from_toml(text).unwrap_err().to_string();
        assert!(error("[[rule]]\nadd = [\"x\"]").contains("missing field `id`"));
        assert!(
            error("id = \"test:t\"\n[[rule]]\nadd = [\"x\"]\nversions = []").contains("versions")
        );
        assert!(
            error("id = \"test:t\"\n[[rule]]\nids = [\"1.0\"]").contains("rule 1: sets neither")
        );
        assert!(
            error("id = \"test:t\"\n[[rule]]\nadd = [\"x\"]\n[[rule]]\nadd = [\"y\"]\nremove = [\"y\"]")
                .contains("rule 2: both adds and removes `y`")
        );
        assert!(error("id = \"test:t\"\n[[rule]]\nadd = [\"a b\"]").contains("invalid trait name"));
        assert!(error("id = \"nope\"").contains("id `nope`"));
        assert!(
            error("id = \"test:t\"\n[[rule]]\nadd = [\"x\"]\nsources = [\"bad\"]")
                .contains("sources")
        );
    }
}
