//! Services that coblet crates receive by injection, and the infrastructure
//! built on them.
//!
//! Business crates never open a socket, touch the file system or read the
//! clock directly. They take these traits as `Arc<dyn Trait>`:
//!
//! - [`Transport`] for HTTP, with [`Request`] / [`Response`] and a streaming [`Body`];
//!   `ReqwestTransport` (feature `reqwest`) is the default client
//! - [`Storage`] for files, directories and advisory locks, with `TokioStorage` (feature `fs`)
//! - [`SecretStore`] for tokens, with [`MemorySecretStore`] as the built-in store
//! - [`Clock`] for wall-clock time
//! - [`PlatformProbe`] for describing the current machine
//! - [`ProcessSpawner`] for child processes, with `TokioSpawner` (feature `process`)
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
mod hashing;
mod ids;
mod operation;
mod plan;
mod platform_probe;
mod process;
mod secret;
mod storage;
mod transport;

pub use cancel::{CancellationToken, Cancelled, ensure_not_cancelled};
pub use clock::{Clock, FixedClock, SystemClock};
pub use events::{
    AuthPrompt, DEFAULT_PROGRESS_INTERVAL, Event, EventBus, EventSubscription, GameLogLine,
    LogLevel, LogSource, OperationOutcome, ProgressThrottle, Received, StepOutcome, Warning,
    WarningCode,
};
pub use extension::{Extension, Registry, RegistryError};
pub use future::BoxFuture;
pub use hashing::{ComputedHashes, StreamHasher, VerifyError};
pub use ids::{IdError, OperationId, PlanId, StepId};
pub use operation::{
    DEFAULT_STEP_CONCURRENCY, DownloadHandler, ExecutionReport, ExtractHandler, OperationError,
    OperationEvents, OperationHandle, OperationProgress, Operations, ProcessHandler, StepContext,
    StepError,
};
pub use plan::{
    ArtifactKind, CustomStep, DownloadStep, Expectation, ExpectedOutput, ExtractStep, MetadataStep,
    MoveStep, PathRoot, Plan, PlanKind, PlanPath, Precondition, ProcessArg, ProcessStep, Program,
    RelPath, RelPathError, RemoveMode, RemoveStep, Sources, Step, StepGraph, StepGraphError,
    StepNode, VerifyHashStep, WriteFileStep,
};
pub use platform_probe::{PlatformProbe, ProbeError, StdPlatformProbe};
#[cfg(feature = "process")]
pub use process::TokioSpawner;
pub use process::{
    CapturedOutput, KillHandle, OutputStream, ProcessError, ProcessExit, ProcessSpawner,
    ProcessSpec, SpawnedProcess,
};
pub use secret::{
    MemorySecretStore, Secret, SecretKey, SecretKeyError, SecretStore, SecretStoreError,
};
#[cfg(feature = "fs")]
pub use storage::TokioStorage;
pub use storage::{
    DirEntry, EntryKind, FileMetadata, FileReader, FileWriter, LockGuard, Storage, StorageError,
};
#[cfg(feature = "reqwest")]
pub use transport::ReqwestTransport;
pub use transport::{
    Body, HttpUrl, HttpUrlError, MemoryBody, Method, Request, Response, Transport, TransportError,
    UserAgent, UserAgentError,
};
