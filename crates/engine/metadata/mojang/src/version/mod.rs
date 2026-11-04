//! Version JSON (`client.json`) structures, the rules engine and `inheritsFrom` merging.

mod merge;
pub(crate) mod raw;
mod rules;

pub use merge::InheritError;
pub use raw::{
    Argument, ArgumentValue, Arguments, Artifact, AssetIndexRef, Download, Downloads, Extract,
    JavaVersion, Library, LibraryDownloads, Logging, LoggingConfig, LoggingFile, RawVersionJson,
};
pub use rules::{Condition, FeatureClause, OsCondition, Rule, RuleAction, RuleContext, evaluate};

#[cfg(test)]
pub(crate) use raw::tests;
