//! Services that coblet crates receive by injection instead of reaching for
//! the outside world directly, plus the shared infrastructure built on them.

#![forbid(unsafe_code)]

mod future;

pub use future::BoxFuture;
