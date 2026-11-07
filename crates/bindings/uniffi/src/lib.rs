//! UniFFI bindings for coblet.
//!
//! `CobletBuilder` adds Platform Profiles, launch customizers, a Microsoft
//! Client ID and a secret store; `Coblet` lists versions, manages accounts,
//! skins and capes, and launches games; `LoginFlow` carries a Microsoft
//! login; `GameHandle` streams the log, kills and waits; `EventStream`
//! delivers launch progress, file transfers, game starts, logs and exits. Foreign code can
//! implement `PlatformProfile` to adapt launches to a platform,
//! `LaunchCustomizer` and `LaunchStep` to change single launches, and
//! `SecretStore` to keep tokens in a system keychain, `UrlRewriter` to
//! point requests at mirrors, and `ModLoader` to add a Mod Loader, as Rust
//! code does with the traits of the same names. Mod Loaders can also be
//! described as TOML or JSON data with `CobletBuilder.add_loader_toml`. Async methods run on tokio and
//! appear as `async` in Swift, `suspend` in Kotlin and `asyncio` coroutines
//! in Python. Errors are a single `CobletError` with a kind and a message.

uniffi::setup_scaffolding!();

mod account;
mod accounts;
mod builder;
mod context;
mod customize;
mod error;
mod events;
mod game;
mod loaders;
mod login;
mod profile;
mod secrets;
mod sources;

pub use account::{
    AccountInfo, AccountState, CapeInfo, GameLicense, LoginInteraction, LoginPrompt, PlayerInfo,
    SkinInfo, SkinModel,
};
pub use builder::CobletBuilder;
pub use context::{Coblet, LaunchMode, LaunchOptions, PlatformInfo, VersionInfo};
pub use customize::{LaunchCustomizer, LaunchStep, LibrarySpec, ProfileEditor, StepInfo};
pub use error::{CobletError, ErrorKind};
pub use events::{CobletEvent, EventStream, Outcome, TransferKind, generate_operation_id};
pub use game::{GameExitInfo, GameHandle, LogLine};
pub use loaders::{
    LaunchPlan, LoaderChoice, LoaderContext, LoaderInfo, LoaderVersionInfo, ModLoader,
};
pub use login::LoginFlow;
pub use profile::{
    ComponentInfo, JavaSourceChoice, LibraryInfo, NativesChoice, PlatformProfile, ProfileDraft,
    ProfileTarget, RuleOverrides,
};
pub use secrets::{SecretStore, generate_secret_key};
pub use sources::{ArtifactKind, UrlRewriter};
