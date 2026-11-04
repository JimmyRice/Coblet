//! Services that coblet crates receive by injection instead of reaching for
//! the outside world directly, plus the shared infrastructure built on them.

#![forbid(unsafe_code)]

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
