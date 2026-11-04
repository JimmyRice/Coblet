//! Services that coblet crates receive by injection instead of reaching for
//! the outside world directly, plus the shared infrastructure built on them.

#![forbid(unsafe_code)]

mod clock;
mod events;
mod extension;
mod future;
mod ids;
mod platform_probe;
mod secret;
mod storage;
mod transport;

pub use clock::{Clock, FixedClock, SystemClock};
pub use events::{
    DEFAULT_PROGRESS_INTERVAL, Event, EventBus, EventSubscription, OperationOutcome,
    ProgressThrottle, Received, StepOutcome, Warning, WarningCode,
};
pub use extension::{Extension, Registry, RegistryError};
pub use future::BoxFuture;
pub use ids::{IdError, OperationId, PlanId, StepId};
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
