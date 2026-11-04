//! Operations: execution of Plans with staging, a journal, progress and cancellation.

mod builtin;
mod context;
mod error;
mod executor;
mod handle;
mod handlers;
mod journal;

pub use context::{ExecutionReport, StepContext};
pub use error::{OperationError, StepError};
pub use executor::{DEFAULT_STEP_CONCURRENCY, Operations};
pub use handle::{OperationEvents, OperationHandle, OperationProgress};
pub use handlers::{DownloadHandler, ExtractHandler, ProcessHandler};
