use std::sync::LazyLock;

use coblet_catalog::Timestamp;
use coblet_types::GameVersionId;
use serde::Deserialize;

use super::{VersionTrait, VersionTraits};
use crate::RawVersionJson;

/// Main class of Mojang's launchwrapper.
pub const LAUNCHWRAPPER_MAIN: &str = "net.minecraft.launchwrapper.Launch";

/// Data table of traits that cannot be derived from metadata alone.
///
/// The built-in table is `data/version-traits.toml`, embedded at build time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraitTable {
    /// Tweakers that mark a launchwrapper version as an Applet.
    #[serde(default)]
    applet_tweakers: Vec<String>,
    #[serde(default, rename = "rule")]
    rules: Vec<TableRule>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableRule {
    traits: Vec<VersionTrait>,
    #[serde(default)]
    ids: Vec<GameVersionId>,
    #[serde(default)]
    released_before: Option<Timestamp>,
    #[serde(default)]
    released_after: Option<Timestamp>,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    #[allow(dead_code, reason = "documents the rule in the data file")]
    note: Option<String>,
}

const BUILTIN_TABLE: &str = include_str!("../../data/version-traits.toml");

static BUILTIN: LazyLock<TraitTable> = LazyLock::new(|| {
    TraitTable::parse(BUILTIN_TABLE).unwrap_or_else(|error| {
        tracing::error!(%error, "the built-in version trait table is invalid");
        TraitTable::default()
    })
});

/// What a [`TraitTable`] rule is checked against.
struct Facts<'a> {
    id: &'a GameVersionId,
    released: Option<Timestamp>,
    assets: Option<&'a str>,
}

impl TableRule {
    fn applies(&self, facts: &Facts<'_>) -> bool {
        if !self.ids.is_empty() && !self.ids.contains(facts.id) {
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
        self.assets.is_empty()
            || facts
                .assets
                .is_some_and(|assets| self.assets.iter().any(|known| known == assets))
    }
}

impl TraitTable {
    /// Returns the built-in table.
    #[must_use]
    pub fn builtin() -> &'static Self {
        &BUILTIN
    }

    /// Parses a table in the format of `data/version-traits.toml`.
    ///
    /// # Errors
    ///
    /// Returns the [`toml::de::Error`] of malformed tables.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Returns the traits known from the version manifest alone: rules that
    /// need the version JSON are skipped.
    #[must_use]
    pub fn for_manifest_entry(
        &self,
        id: &GameVersionId,
        released: Option<Timestamp>,
    ) -> VersionTraits {
        self.matching_rules(&Facts {
            id,
            released,
            assets: None,
        })
    }

    /// Returns the traits of a version JSON: derived from its main class,
    /// tweakers and Java runtime, plus the table's rules.
    #[must_use]
    pub fn for_version(&self, raw: &RawVersionJson) -> VersionTraits {
        let mut traits = self.matching_rules(&Facts {
            id: &raw.id,
            released: raw.release_time,
            assets: raw
                .asset_index
                .as_ref()
                .map(|index| index.id.as_str())
                .or(raw.assets.as_deref()),
        });
        if raw.main_class.as_deref() == Some(LAUNCHWRAPPER_MAIN)
            && raw
                .tweak_classes()
                .iter()
                .any(|tweaker| self.applet_tweakers.iter().any(|known| known == tweaker))
        {
            traits.insert(VersionTrait::Applet);
        }
        if raw
            .java_version
            .as_ref()
            .is_some_and(|java| java.component == "jre-legacy")
        {
            traits.insert(VersionTrait::JreLegacy);
        }
        traits
    }

    fn matching_rules(&self, facts: &Facts<'_>) -> VersionTraits {
        self.rules
            .iter()
            .filter(|rule| rule.applies(facts))
            .flat_map(|rule| rule.traits.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use coblet_types::GameVersionId;

    use super::{TraitTable, VersionTrait, VersionTraits};
    use crate::RawVersionJson;

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
        let table = TraitTable::parse(super::BUILTIN_TABLE).unwrap();
        assert_eq!(&table, TraitTable::builtin());
        assert_eq!(table.applet_tweakers.len(), 2);
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
    fn rejects_unknown_fields() {
        assert!(TraitTable::parse("[[rule]]\ntraits = []\nversions = []").is_err());
        let table = TraitTable::parse("[[rule]]\ntraits = [\"acme-x\"]\nids = [\"1.0\"]").unwrap();
        let id = GameVersionId::new("1.0").unwrap();
        assert_eq!(names(&table.for_manifest_entry(&id, None)), ["acme-x"]);
    }
}
