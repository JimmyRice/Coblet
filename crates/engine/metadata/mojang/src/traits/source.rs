use coblet_runtime::Extension;
use coblet_types::GameVersionId;

use super::VersionTraits;

/// Extension that adds or removes traits, for example from another launcher's metadata.
///
/// Sources run after the built-in derivation, in registration order.
pub trait TraitSource: Extension {
    /// Changes the traits of a version.
    fn adjust(&self, version: &GameVersionId, traits: &mut VersionTraits);
}
