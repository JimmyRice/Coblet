use coblet_platform::Component;
use serde::{Deserialize, Serialize};

/// What installing a Mod Loader version adds to an Instance: a declarative
/// description that the engine carries out, so a Mod Loader does no IO of
/// its own beyond reading metadata.
///
/// Components come in stack order. A Mod Loader that needs a mapping layer
/// on obfuscated game versions returns it as its own component before the
/// loader, and leaves it out where the metadata has none.
///
/// In JSON, as foreign Mod Loaders return it, a recipe is
/// `{"components": [...]}` where each component is a
/// [`ComponentPatch`](coblet_platform::ComponentPatch) in the Prism format,
/// optionally with `"dependencyOnly": true`.
///
/// # Examples
///
/// ```
/// use coblet_loader::InstallRecipe;
///
/// let recipe = InstallRecipe::from_json(br#"{ "components": [
///     { "uid": "net.fabricmc.intermediary", "version": "1.20.1", "dependencyOnly": true },
///     { "uid": "net.fabricmc.fabric-loader", "version": "0.16.9",
///       "requires": [{ "uid": "net.fabricmc.intermediary" }] }
/// ] }"#)?;
/// assert_eq!(recipe.components.len(), 2);
/// assert!(recipe.component("net.fabricmc.intermediary").unwrap().dependency_only);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InstallRecipe {
    /// Components to add, in stack order.
    pub components: Vec<Component>,
}

impl InstallRecipe {
    /// Creates a recipe that adds these components.
    #[must_use]
    pub fn new(components: Vec<Component>) -> Self {
        Self { components }
    }

    /// Parses a recipe from JSON.
    ///
    /// # Errors
    ///
    /// Returns the parser's error when the JSON is malformed.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Returns the component with this UID.
    #[must_use]
    pub fn component(&self, uid: &str) -> Option<&Component> {
        self.components
            .iter()
            .find(|component| component.uid() == uid)
    }
}
