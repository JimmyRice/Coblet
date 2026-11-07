//! Mojang metadata for coblet.
//!
//! - [`MojangVersions`] is the catalog source for `version_manifest_v2.json`;
//!   it lists [`GameVersion`]s newest first and marks the latest release as
//!   recommended.
//! - [`RawVersionJson`] is the parsed `client.json`, kept with its rules
//!   unevaluated as the input of the Profile pipeline. [`evaluate`] decides
//!   OS conditions for a [`RuleContext`] and leaves launch features open.
//! - [`AssetIndex`] lists the asset objects of a version.
//! - [`VersionTraits`] name the concrete problems of old versions. An
//!   ordered list of [`TraitSource`]s, [`TraitSources`], gives them to
//!   versions: the built-in [`TraitTable`] first, then tables and code
//!   sources added by the launcher, such as an [`FnTraitSource`].
//!
//! Mapping downloads are optional throughout: from 26.1 on, versions are
//! not obfuscated and ship no mappings.
//!
//! ```
//! use coblet_mojang::{RawVersionJson, RuleContext, evaluate};
//!
//! let raw = RawVersionJson::from_slice(br#"{"id": "x", "libraries": [
//!     {"name": "org.lwjgl:lwjgl:3.3.1:natives-macos-arm64",
//!      "rules": [{"action": "allow", "os": {"name": "osx"}}]}
//! ]}"#)?;
//! let linux = RuleContext::new("linux", "x86_64");
//! assert!(!evaluate(&raw.libraries[0].rules, &linux).is_allowed_by_default());
//! # Ok::<(), serde_json::Error>(())
//! ```

#![forbid(unsafe_code)]

mod assets;
mod manifest;
mod traits;
mod version;

pub use assets::{AssetIndex, AssetObject, RESOURCES_URL};
pub use manifest::{
    Document, GameVersion, Latest, MANIFEST_URL, ManifestEntry, MojangVersions, VersionManifest,
    fetch_asset_index, fetch_for_source, fetch_version_json,
};
pub use traits::{
    BUILTIN_TRAITS_TOML, FnTraitSource, LAUNCHWRAPPER_MAIN, TRAIT_SOURCE_FAILED, TraitAction,
    TraitChange, TraitContext, TraitError, TraitFailure, TraitInput, TraitReport, TraitSource,
    TraitSources, TraitTable, TraitTarget, VersionTrait, VersionTraits,
};
pub use version::{
    Argument, ArgumentValue, Arguments, Artifact, AssetIndexRef, Condition, Download, Downloads,
    Extract, FeatureClause, InheritError, JavaVersion, Library, LibraryDownloads, Logging,
    LoggingConfig, LoggingFile, OsCondition, RawVersionJson, Rule, RuleAction, RuleContext,
    evaluate,
};
