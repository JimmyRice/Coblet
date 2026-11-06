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
#[path = "../tests/traits.rs"]
mod tests;
