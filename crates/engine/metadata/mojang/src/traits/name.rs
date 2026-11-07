use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

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
