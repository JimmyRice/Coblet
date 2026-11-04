//! Services that coblet crates receive by injection, and the infrastructure
//! built on them.
//!
//! Business crates never open a socket, touch the file system or read the
//! clock directly. They take these traits as `Arc<dyn Trait>`:
//!
//! - [`Transport`] for HTTP, with [`Request`] / [`Response`] and a streaming [`Body`]
//! - [`Storage`] for files, directories and advisory locks
//! - [`SecretStore`] for tokens, with [`MemorySecretStore`] as the built-in store
//! - [`Clock`] for wall-clock time
//! - [`PlatformProbe`] for describing the current machine
//!
//! The same crate holds the pieces shared by every subsystem: [`Registry`]
//! for extension points keyed by [`coblet_types::ExtensionId`], the
//! [`EventBus`] for progress and warnings, [`CancellationToken`], and the
//! [`Plan`] model ([`StepGraph`], [`Step`], [`Precondition`]) that every
//! change goes through before it touches disk.
//!
//! Methods of these traits return [`BoxFuture`] so the traits stay object safe.
//!
//! ```
//! use std::sync::Arc;
//!
//! use coblet_runtime::{Clock, MemorySecretStore, Secret, SecretKey, SecretStore, SystemClock};
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let clock: Arc<dyn Clock> = Arc::new(SystemClock);
//! let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
//!
//! let key = SecretKey::new("acme:auth/alice/refresh_token")?;
//! secrets.put(&key, Secret::new("token")).await?;
//! assert_eq!(secrets.get(&key).await?.map(|s| s.expose().to_owned()), Some("token".to_owned()));
//! assert!(clock.now() > std::time::SystemTime::UNIX_EPOCH);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! ```

#![forbid(unsafe_code)]

mod assertions;
mod cancel;
mod clock;
mod events;
mod extension;
mod future;
mod ids;
mod plan;
mod platform_probe;
mod secret;
mod storage;
mod transport;

pub use cancel::{CancellationToken, Cancelled, ensure_not_cancelled};
pub use clock::{Clock, FixedClock, SystemClock};
pub use events::{
    DEFAULT_PROGRESS_INTERVAL, Event, EventBus, EventSubscription, OperationOutcome,
    ProgressThrottle, Received, StepOutcome, Warning, WarningCode,
};
pub use extension::{Extension, Registry, RegistryError};
pub use future::BoxFuture;
pub use ids::{IdError, OperationId, PlanId, StepId};
pub use plan::{
    ArtifactKind, CustomStep, DownloadStep, Expectation, ExpectedOutput, ExtractStep, MetadataStep,
    MoveStep, PathRoot, Plan, PlanKind, PlanPath, Precondition, ProcessArg, ProcessStep, Program,
    RelPath, RelPathError, RemoveMode, RemoveStep, Sources, Step, StepGraph, StepGraphError,
    StepNode, VerifyHashStep, WriteFileStep,
};
pub use platform_probe::{PlatformProbe, ProbeError, StdPlatformProbe};
pub use secret::{
    MemorySecretStore, Secret, SecretKey, SecretKeyError, SecretStore, SecretStoreError,
};
pub use storage::{
    DirEntry, EntryKind, FileMetadata, FileWriter, LockGuard, Storage, StorageError,
};
pub use transport::{
    Body, HttpUrl, HttpUrlError, MemoryBody, Method, Request, Response, Transport, TransportError,
    UserAgent, UserAgentError,
};
