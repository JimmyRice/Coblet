use std::error::Error;

use crate::{PlanId, PlanPath, StepId, StorageError, VerifyError};

/// Failure of a single step.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    /// The Operation was cancelled while the step ran.
    #[error("cancelled")]
    Cancelled,
    /// A file system operation failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A file does not match its expected size or hash.
    #[error("{path} does not match: {source}")]
    Verify {
        /// The file.
        path: PlanPath,
        /// The mismatch.
        #[source]
        source: VerifyError,
    },
    /// A precondition no longer holds; another program changed the file.
    #[error("{path} changed since the plan was made")]
    PlanStale {
        /// The changed file.
        path: PlanPath,
    },
    /// The step uses `target:` paths but the Operation has no target directory.
    #[error("the step writes to the target directory, but the operation has none")]
    MissingTarget,
    /// No handler is installed for this kind of step.
    #[error("no handler is installed for {0} steps")]
    NoHandler(&'static str),
    /// Any other failure, reported by a handler or custom step.
    #[error(transparent)]
    Other(Box<dyn Error + Send + Sync>),
}

impl StepError {
    /// Wraps a handler-specific error.
    pub fn other(error: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self::Other(error.into())
    }
}

impl From<crate::Cancelled> for StepError {
    fn from(_: crate::Cancelled) -> Self {
        Self::Cancelled
    }
}

/// Failure of a whole Operation.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    /// A precondition failed before anything was written.
    #[error("plan is stale: {path} changed since the plan was made")]
    PlanStale {
        /// The changed file.
        path: PlanPath,
    },
    /// A step failed; staged changes were rolled back.
    #[error("{step} ({label}) failed: {source}")]
    Step {
        /// The failed step.
        step: StepId,
        /// Its label.
        label: String,
        /// Why it failed.
        #[source]
        source: StepError,
    },
    /// The Operation was cancelled; staged changes were rolled back.
    #[error("operation was cancelled")]
    Cancelled,
    /// Setting up staging or the journal failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// No saved Plan exists for this ID, or it cannot be read.
    #[error("plan {plan} cannot be resumed: {reason}")]
    NotResumable {
        /// The Plan.
        plan: PlanId,
        /// Why.
        reason: String,
    },
    /// The task running the Operation stopped without a result.
    #[error("operation task stopped unexpectedly")]
    Aborted,
}

impl OperationError {
    /// Reports whether the Operation stopped because it was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            Self::Cancelled
                | Self::Step {
                    source: StepError::Cancelled,
                    ..
                }
        )
    }
}
