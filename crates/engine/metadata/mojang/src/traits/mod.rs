//! Version traits: names, sources and the data table.

mod name;
mod source;
mod sources;
mod table;
#[cfg(test)]
pub(crate) mod testing;

pub use name::{VersionTrait, VersionTraits};
pub use source::{
    TraitAction, TraitChange, TraitContext, TraitError, TraitFailure, TraitInput, TraitReport,
    TraitSource, TraitTarget,
};
pub use sources::{TRAIT_SOURCE_FAILED, TraitSources};
pub use table::{LAUNCHWRAPPER_MAIN, TraitTable};
