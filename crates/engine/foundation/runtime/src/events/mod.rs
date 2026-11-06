//! Event stream for progress, step results, warnings, launches and game logs.

mod auth;
mod bus;
mod event;
mod log;
mod throttle;
mod transfer;

pub use auth::AuthPrompt;
pub use bus::{EventBus, EventSubscription, Received};
pub use event::{Event, OperationOutcome, StepOutcome, Warning, WarningCode};
pub use log::{GameLogLine, LogLevel, LogSource};
pub use throttle::{DEFAULT_PROGRESS_INTERVAL, ProgressThrottle};
pub use transfer::{TransferKind, TransferProgress, TransferSnapshot};
