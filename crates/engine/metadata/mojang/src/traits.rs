use std::collections::BTreeSet;
use std::fmt;
use std::sync::LazyLock;

use coblet_catalog::Timestamp;
use coblet_runtime::Extension;
use coblet_types::GameVersionId;
use serde::{Deserialize, Serialize};

use crate::RawVersionJson;

/// Main class of Mojang's launchwrapper.
pub const LAUNCHWRAPPER_MAIN: &str = "net.minecraft.launchwrapper.Launch";

/// Fact about a version that needs special handling or a diagnostic.
///
/// Each trait names one concrete problem; there is no general "old version"
/// trait because old versions differ in which problems they have.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum VersionTrait {
    /// The game is an Applet; launchwrapper provides the window and passes the arguments.
    Applet,
    /// The game only runs on Java 8.
    JreLegacy,
    /// This Forge version downloads libraries from a retired server at startup.
    LegacyFmlLibs,
    /// Skins, sounds or other features depend on retired online services.
    LegacyOnlineServices,
    /// Known rendering problems on current macOS, such as swapped colors on Apple Silicon.
    LegacyMacosRendering,
    /// A trait defined by an extension, stored verbatim.
    Other(String),
}

impl VersionTrait {
    /// Returns the trait name, such as `jre-legacy`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Applet => "applet",
            Self::JreLegacy => "jre-legacy",
            Self::LegacyFmlLibs => "legacy-fml-libs",
            Self::LegacyOnlineServices => "legacy-online-services",
            Self::LegacyMacosRendering => "legacy-macos-rendering",
            Self::Other(other) => other,
        }
    }
}

impl From<&str> for VersionTrait {
    fn from(value: &str) -> Self {
        match value {
            "applet" => Self::Applet,
            "jre-legacy" => Self::JreLegacy,
            "legacy-fml-libs" => Self::LegacyFmlLibs,
            "legacy-online-services" => Self::LegacyOnlineServices,
            "legacy-macos-rendering" => Self::LegacyMacosRendering,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl From<String> for VersionTrait {
    fn from(value: String) -> Self {
        match Self::from(value.as_str()) {
            Self::Other(_) => Self::Other(value),
            known => known,
        }
    }
}

impl From<VersionTrait> for String {
    fn from(value: VersionTrait) -> Self {
        match value {
            VersionTrait::Other(other) => other,
            known => known.as_str().to_owned(),
        }
    }
}

impl fmt::Display for VersionTrait {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Set of [`VersionTrait`]s, ordered and free of duplicates.
///
/// # Examples
///
/// ```
/// use coblet_mojang::{VersionTrait, VersionTraits};
///
/// let mut traits = VersionTraits::new();
/// traits.insert(VersionTrait::JreLegacy);
/// traits.insert(VersionTrait::from("acme-needs-proxy"));
/// assert!(traits.contains(&VersionTrait::JreLegacy));
/// assert_eq!(traits.to_string(), "jre-legacy, acme-needs-proxy");
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionTraits(BTreeSet<VersionTrait>);

impl VersionTraits {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a trait; returns whether it was new.
    pub fn insert(&mut self, value: VersionTrait) -> bool {
        self.0.insert(value)
    }

    /// Removes a trait; returns whether it was present.
    pub fn remove(&mut self, value: &VersionTrait) -> bool {
        self.0.remove(value)
    }

    /// Reports whether a trait is present.
    #[must_use]
    pub fn contains(&self, value: &VersionTrait) -> bool {
        self.0.contains(value)
    }

    /// Iterates over the traits in order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &VersionTrait> {
        self.0.iter()
    }

    /// Returns the number of traits.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Reports whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Extend<VersionTrait> for VersionTraits {
    fn extend<I: IntoIterator<Item = VersionTrait>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl FromIterator<VersionTrait> for VersionTraits {
    fn from_iter<I: IntoIterator<Item = VersionTrait>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl fmt::Display for VersionTraits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, value) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            f.write_str(value.as_str())?;
        }
        Ok(())
    }
}

/// Extension that adds or removes traits, for example from another launcher's metadata.
///
/// Sources run after the built-in derivation, in registration order.
pub trait TraitSource: Extension {
    /// Changes the traits of a version.
    fn adjust(&self, version: &GameVersionId, traits: &mut VersionTraits);
}

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

const BUILTIN_TABLE: &str = include_str!("../data/version-traits.toml");

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
