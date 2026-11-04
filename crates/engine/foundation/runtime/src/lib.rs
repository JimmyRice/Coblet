//! Services that coblet crates receive by injection instead of reaching for
//! the outside world directly, plus the shared infrastructure built on them.

#![forbid(unsafe_code)]

mod clock;
mod extension;
mod future;

pub use clock::{Clock, FixedClock, SystemClock};
pub use extension::{Extension, Registry, RegistryError};
pub use future::BoxFuture;
