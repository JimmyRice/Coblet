use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::OperationProgress;
use super::error::StepError;
use crate::{
    Event, EventBus, OperationId, PathRoot, PlanId, PlanPath, ProgressThrottle, StepId,
    StepOutcome, Storage, Warning, WarningCode,
};

/// State shared by every step of one Operation.
pub(super) struct Shared {
    pub(super) op: OperationId,
    pub(super) plan: PlanId,
    pub(super) storage: Arc<dyn Storage>,
    pub(super) data_root: PathBuf,
    pub(super) staging_root: PathBuf,
    pub(super) target: Option<PathBuf>,
    pub(super) cancel: CancellationToken,
    pub(super) events: EventBus,
    pub(super) progress: Mutex<ProgressState>,
    pub(super) progress_tx: watch::Sender<OperationProgress>,
}

/// Byte counts per step plus the throttle for progress events.
pub(super) struct ProgressState {
    pub(super) per_step: HashMap<StepId, (u64, Option<u64>)>,
    pub(super) throttle: ProgressThrottle,
}

impl Shared {
    pub(super) fn resolve(&self, path: &PlanPath) -> Result<PathBuf, StepError> {
        let root = match path.root {
            PathRoot::Staging => &self.staging_root,
            PathRoot::Data => &self.data_root,
            PathRoot::Target => self.target.as_ref().ok_or(StepError::MissingTarget)?,
        };
        Ok(path.path.under(root))
    }

    pub(super) fn update_progress(&self, edit: impl FnOnce(&mut OperationProgress)) {
        self.progress_tx.send_modify(edit);
    }
}

/// What a step sees while it runs: storage, resolved directories, cancellation and progress.
///
/// The executor creates one per step; handlers for download, extract and
/// process steps and [`CustomStep`](crate::CustomStep)s receive it.
pub struct StepContext {
    pub(super) shared: Arc<Shared>,
    pub(super) step: StepId,
}

impl StepContext {
    /// Returns the Operation running the step.
    #[must_use]
    pub fn operation(&self) -> OperationId {
        self.shared.op
    }

    /// Returns the Plan being executed.
    #[must_use]
    pub fn plan(&self) -> PlanId {
        self.shared.plan
    }

    /// Returns the step being run.
    #[must_use]
    pub fn step(&self) -> StepId {
        self.step
    }

    /// Returns the storage the Operation writes through.
    #[must_use]
    pub fn storage(&self) -> &dyn Storage {
        &*self.shared.storage
    }

    /// Returns the SDK data root.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.shared.data_root
    }

    /// Resolves a Plan path against the staging, data and target directories.
    ///
    /// # Errors
    ///
    /// Returns [`StepError::MissingTarget`] for a `target:` path when the
    /// Operation has no target directory.
    pub fn resolve(&self, path: &PlanPath) -> Result<PathBuf, StepError> {
        self.shared.resolve(path)
    }

    /// Returns the token that fires when the Operation is cancelled.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        &self.shared.cancel
    }

    /// Returns [`StepError::Cancelled`] once the Operation is cancelled; call it at IO boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`StepError::Cancelled`] when the Operation was cancelled.
    pub fn ensure_not_cancelled(&self) -> Result<(), StepError> {
        if self.shared.cancel.is_cancelled() {
            Err(StepError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Records that `done` of `total` units (usually bytes) of this step are complete.
    ///
    /// The Operation's aggregate progress is updated at once; a
    /// [`Event::StepProgress`] goes out at most 30 times per second per Operation.
    pub fn report_progress(&self, done: u64, total: Option<u64>) {
        let publish = {
            let Ok(mut state) = self.shared.progress.lock() else {
                return;
            };
            state.per_step.insert(self.step, (done, total));
            let bytes_done = state.per_step.values().map(|(d, _)| d).sum::<u64>();
            self.shared.update_progress(|p| p.bytes_done = bytes_done);
            state.throttle.allow(Instant::now(), done, total)
        };
        if publish {
            self.shared.events.publish(Event::StepProgress {
                op: self.shared.op,
                step: self.step,
                done,
                total,
            });
        }
    }

    /// Publishes a warning attached to the Operation.
    pub fn warn(&self, code: WarningCode, message: impl Into<String>) {
        self.shared.events.publish(Event::Warning(Warning {
            op: Some(self.shared.op),
            code,
            message: message.into(),
        }));
    }
}

/// Outcome of every step of a finished Operation, handed to [`PlanKind::finish`](crate::PlanKind::finish).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionReport {
    /// The Operation.
    pub op: OperationId,
    /// The executed Plan.
    pub plan: PlanId,
    /// How each step ended, in completion order. Steps finished before a resume are
    /// listed as [`StepOutcome::Skipped`].
    pub steps: Vec<(StepId, StepOutcome)>,
}

impl ExecutionReport {
    /// Returns the outcome of one step.
    #[must_use]
    pub fn outcome(&self, step: StepId) -> Option<&StepOutcome> {
        self.steps
            .iter()
            .find(|(id, _)| *id == step)
            .map(|(_, outcome)| outcome)
    }
}
