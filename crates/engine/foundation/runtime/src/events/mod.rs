//! Event stream for progress, step results, warnings and game logs.

mod auth;
mod bus;
mod event;
mod log;
mod throttle;

pub use auth::AuthPrompt;
pub use bus::{EventBus, EventSubscription, Received};
pub use event::{Event, OperationOutcome, StepOutcome, Warning, WarningCode};
pub use log::{GameLogLine, LogLevel, LogSource};
pub use throttle::{DEFAULT_PROGRESS_INTERVAL, ProgressThrottle};
