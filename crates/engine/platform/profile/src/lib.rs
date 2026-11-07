//! Platform Profile engine for coblet.
//!
//! `client.json` describes the game for the three platforms Mojang supports.
//! [`ProfileEngine::resolve`] turns a platform-neutral [`ResolvedProfile`]
//! into an [`EffectiveLaunchProfile`] for one machine:
//!
//! 1. **Match**: select the [`PlatformProfile`]s for the platform, version and components.
//! 2. **Evaluate**: decide Mojang rules with the OS name and architecture the profiles map the platform onto.
//! 3. **Rewrite**: apply library, native, argument, environment and Java changes.
//! 4. **Validate**: report [`ProfileError::Unsupported`] with what is missing
//!    instead of borrowing another platform's natives.
//!
//! Profiles are data ([`DataProfile`], TOML) or code; the built-in ones are
//! TOML files embedded from `data/profiles/`. [`EffectiveLaunchProfile::explain`]
//! lists which profile changed what.

#![forbid(unsafe_code)]

mod builtin;
mod component;
mod compose;
mod data;
mod edit;
mod effective;
mod engine;
mod error;
mod pattern;
mod profile;
mod resolved;

pub use component::{Agent, Component, ComponentPatch, Requirement};
pub use data::DataProfile;
pub use edit::{DetachedDraft, NativesSource, ProfileEdit};
pub use effective::{
    Change, ChangeSource, EffectiveLaunchProfile, EffectiveLibrary, EnvValue, FileSource,
    JavaMatch, JavaRequirement, JavaSource, LibraryFile, NativeJar, Natives, PipelineStep,
    ProfileArg, ProfileWarning,
};
pub use engine::{LIBRARIES_URL, ProfileEngine};
pub use error::{Missing, ProfileError};
pub use pattern::{compare_versions, glob_matches};
pub use profile::{
    Draft, PlatformProfile, ProfileLayer, RuleSettings, Target, auth_argument_change,
    is_auth_argument,
};
pub use resolved::{ComponentRef, LWJGL2_UID, LWJGL3_UID, MINECRAFT_UID, ResolvedProfile};
