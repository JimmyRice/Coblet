use std::str::FromStr as _;

use coblet::runtime::{
    Event, EventSubscription, OperationId, OperationOutcome, Received, StepOutcome,
};

use crate::game::log_line;
use crate::{Coblet, CobletError, ErrorKind, LogLine, LoginPrompt};

/// Group of files a transfer fetches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferKind {
    /// Libraries, the client jar, the asset index and the log configuration.
    GameFiles,
    /// Asset objects: sounds, languages and textures.
    Assets,
    /// Files of a Java runtime.
    JavaRuntime,
    /// Anything else.
    Other,
}

impl From<coblet::runtime::TransferKind> for TransferKind {
    fn from(kind: coblet::runtime::TransferKind) -> Self {
        use coblet::runtime::TransferKind as Kind;
        match kind {
            Kind::GameFiles => Self::GameFiles,
            Kind::Assets => Self::Assets,
            Kind::JavaRuntime => Self::JavaRuntime,
            _ => Self::Other,
        }
    }
}

/// How an Operation, a launch or a step ended.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Outcome {
    /// It succeeded.
    Succeeded,
    /// A step had nothing to do.
    Skipped,
    /// It failed.
    Failed {
        /// Why.
        message: String,
    },
    /// It was cancelled.
    Cancelled,
}

impl From<OperationOutcome> for Outcome {
    fn from(outcome: OperationOutcome) -> Self {
        match outcome {
            OperationOutcome::Succeeded => Self::Succeeded,
            OperationOutcome::Failed { message } => Self::Failed { message },
            _ => Self::Cancelled,
        }
    }
}

impl From<StepOutcome> for Outcome {
    fn from(outcome: StepOutcome) -> Self {
        match outcome {
            StepOutcome::Succeeded => Self::Succeeded,
            StepOutcome::Skipped => Self::Skipped,
            StepOutcome::Failed { message } => Self::Failed { message },
            _ => Self::Cancelled,
        }
    }
}

/// Something coblet reports while it works. `op` is the ID of the
/// Operation or launch the event belongs to.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CobletEvent {
    /// An Operation or a launch (`kind` is `launch`) started.
    OperationStarted {
        /// Operation ID.
        op: String,
        /// Kind, such as `launch` or `install`.
        kind: String,
    },
    /// An Operation or a launch ended; a launch ends once the game runs.
    OperationFinished {
        /// Operation ID.
        op: String,
        /// How it ended.
        outcome: Outcome,
    },
    /// A launch began a step, such as `verify-files`.
    LaunchStep {
        /// Launch ID.
        op: String,
        /// Step name.
        step: String,
        /// Position of the step, from 0.
        index: u32,
        /// Number of steps.
        count: u32,
    },
    /// A batch of files made progress; at most 30 per second per batch.
    Transfer {
        /// Operation or launch ID.
        op: String,
        /// Which files.
        kind: TransferKind,
        /// Files finished.
        files_done: u64,
        /// Files in the batch.
        files_total: u64,
        /// Bytes received or found present.
        bytes_done: u64,
        /// Bytes in the batch, when every size is known.
        bytes_total: Option<u64>,
    },
    /// A step of an Operation made progress.
    StepProgress {
        /// Operation ID.
        op: String,
        /// Step index in the Plan.
        step: u32,
        /// Units done, usually bytes.
        done: u64,
        /// Total units, when known.
        total: Option<u64>,
    },
    /// A step of an Operation ended.
    StepFinished {
        /// Operation ID.
        op: String,
        /// Step index in the Plan.
        step: u32,
        /// How it ended.
        outcome: Outcome,
    },
    /// The game process started.
    GameStarted {
        /// Launch ID.
        op: String,
        /// Process ID, when known.
        pid: Option<u32>,
    },
    /// A line of game output.
    GameLog {
        /// Launch ID.
        op: String,
        /// The line.
        line: LogLine,
    },
    /// The game process ended.
    GameExited {
        /// Launch ID.
        op: String,
        /// Exit code, when the process exited normally.
        code: Option<i32>,
        /// Signal that ended the process on Unix.
        signal: Option<i32>,
        /// Whether the launcher killed the game.
        killed: bool,
        /// Whether the game probably crashed.
        suspected_crash: bool,
    },
    /// Something went wrong without stopping the work.
    Warning {
        /// Operation ID, if the warning belongs to one.
        op: Option<String>,
        /// Stable code such as `catalog.offline-cache`.
        code: String,
        /// Description.
        message: String,
    },
    /// A login waits for the user.
    LoginPrompt {
        /// What to show.
        prompt: LoginPrompt,
    },
    /// The reader fell behind and this many events were dropped.
    Missed {
        /// Dropped events.
        count: u64,
    },
}

/// Converts an engine event; `None` for kinds this binding does not know.
fn convert(event: Event) -> Option<CobletEvent> {
    let id = |op: OperationId| op.to_string();
    Some(match event {
        Event::OperationStarted { op, kind } => CobletEvent::OperationStarted {
            op: id(op),
            kind: kind.to_owned(),
        },
        Event::OperationFinished { op, outcome } => CobletEvent::OperationFinished {
            op: id(op),
            outcome: outcome.into(),
        },
        Event::LaunchStep {
            op,
            step,
            index,
            count,
        } => CobletEvent::LaunchStep {
            op: id(op),
            step,
            index,
            count,
        },
        Event::Transfer { op, kind, progress } => CobletEvent::Transfer {
            op: id(op),
            kind: kind.into(),
            files_done: progress.files_done,
            files_total: progress.files_total,
            bytes_done: progress.bytes_done,
            bytes_total: progress.bytes_total,
        },
        Event::StepProgress {
            op,
            step,
            done,
            total,
        } => CobletEvent::StepProgress {
            op: id(op),
            step: step.index(),
            done,
            total,
        },
        Event::StepFinished { op, step, outcome } => CobletEvent::StepFinished {
            op: id(op),
            step: step.index(),
            outcome: outcome.into(),
        },
        Event::GameStarted { op, pid } => CobletEvent::GameStarted { op: id(op), pid },
        Event::GameLog { op, line } => CobletEvent::GameLog {
            op: id(op),
            line: log_line(line),
        },
        Event::GameExited {
            op,
            exit,
            killed,
            suspected_crash,
        } => CobletEvent::GameExited {
            op: id(op),
            code: exit.and_then(|exit| exit.code),
            signal: exit.and_then(|exit| exit.signal),
            killed,
            suspected_crash,
        },
        Event::Warning(warning) => CobletEvent::Warning {
            op: warning.op.map(id),
            code: warning.code.as_str().to_owned(),
            message: warning.message,
        },
        Event::AuthPrompt(prompt) => CobletEvent::LoginPrompt {
            prompt: LoginPrompt::from(&prompt),
        },
        _ => return None,
    })
}

/// Events published after `Coblet.subscribe`, read one at a time.
///
/// Reading is pulled by the caller: wrap `next` in an async iterator, a
/// Flow or an `AsyncSequence`. Progress is throttled to 30 events per
/// second per batch, and a reader that falls behind gets a `Missed` event
/// instead of slowing coblet down.
#[derive(uniffi::Object)]
pub struct EventStream {
    inner: tokio::sync::Mutex<EventSubscription>,
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").finish_non_exhaustive()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl EventStream {
    /// Waits for the next event; `None` once the `Coblet` is gone.
    pub async fn next(&self) -> Option<CobletEvent> {
        let mut inner = self.inner.lock().await;
        loop {
            match inner.recv().await? {
                Received::Missed(count) => return Some(CobletEvent::Missed { count }),
                Received::Event(event) => {
                    if let Some(event) = convert(event) {
                        return Some(event);
                    }
                }
            }
        }
    }
}

#[uniffi::export]
impl Coblet {
    /// Starts receiving events: launch steps and file transfers, game
    /// starts, log lines and exits, warnings and login prompts. Events are
    /// built only while some stream exists; drop streams you no longer read.
    #[must_use]
    pub fn subscribe(&self) -> std::sync::Arc<EventStream> {
        std::sync::Arc::new(EventStream {
            inner: tokio::sync::Mutex::new(self.inner.subscribe()),
        })
    }
}

/// Returns a new random operation ID, to pass as a launch's `operation_id`
/// so its events can be matched before the launch call returns.
#[uniffi::export]
#[must_use]
pub fn generate_operation_id() -> String {
    OperationId::random().to_string()
}

/// Parses an operation ID chosen by the caller.
pub(crate) fn operation_id(id: &str) -> Result<OperationId, CobletError> {
    OperationId::from_str(id)
        .map_err(|error| CobletError::new(ErrorKind::InvalidArgument, error.to_string()))
}

#[cfg(test)]
#[path = "../tests/events.rs"]
mod tests;
