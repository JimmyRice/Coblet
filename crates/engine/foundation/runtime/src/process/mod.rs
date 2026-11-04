//! Child processes: the [`ProcessSpawner`] trait and its default implementation.

mod api;
#[cfg(feature = "process")]
mod local;

pub use api::{
    CapturedOutput, KillHandle, OutputStream, ProcessError, ProcessExit, ProcessSpawner,
    ProcessSpec, SpawnedProcess,
};
#[cfg(feature = "process")]
pub use local::TokioSpawner;
