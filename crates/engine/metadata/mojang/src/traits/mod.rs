//! Version traits: names, sources and the data table.

mod name;
mod source;
mod table;

pub use name::{VersionTrait, VersionTraits};
pub use source::TraitSource;
pub use table::{LAUNCHWRAPPER_MAIN, TraitTable};
