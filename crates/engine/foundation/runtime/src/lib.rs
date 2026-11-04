//! Services that coblet crates receive by injection instead of reaching for
//! the outside world directly, plus the shared infrastructure built on them.

#![forbid(unsafe_code)]

mod clock;
mod extension;
mod future;
mod platform_probe;
mod secret;

pub use clock::{Clock, FixedClock, SystemClock};
pub use extension::{Extension, Registry, RegistryError};
pub use future::BoxFuture;
pub use platform_probe::{PlatformProbe, ProbeError, StdPlatformProbe};
pub use secret::{
    MemorySecretStore, Secret, SecretKey, SecretKeyError, SecretStore, SecretStoreError,
};
