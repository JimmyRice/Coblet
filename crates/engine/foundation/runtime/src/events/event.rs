use std::borrow::Cow;
use std::fmt;

use super::{AuthPrompt, GameLogLine};
use crate::{OperationId, StepId};

/// Event published while coblet works: Operation progress, warnings, game
/// logs and login prompts.
///
/// Later milestones add more kinds, which is why the enum is non-exhaustive.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An Operation began executing its Plan.
    OperationStarted {
        /// The Operation.
        op: OperationId,
        /// Kind of Plan being executed, such as `install`.
        kind: &'static str,
    },
    /// A step made progress. Published at most 30 times per second per Operation.
    StepProgress {
        /// The Operation.
        op: OperationId,
        /// The step.
        step: StepId,
        /// Units done so far, usually bytes.
        done: u64,
        /// Total units, when known.
        total: Option<u64>,
    },
    /// A step reached a final state.
    StepFinished {
        /// The Operation.
        op: OperationId,
        /// The step.
        step: StepId,
        /// How the step ended.
        outcome: StepOutcome,
    },
    /// An Operation reached a final state.
    OperationFinished {
        /// The Operation.
        op: OperationId,
        /// How the Operation ended.
        outcome: OperationOutcome,
    },
    /// Something went wrong without stopping the work, such as falling back to cached metadata.
    Warning(Warning),
    /// A line of a running game's output.
    GameLog(GameLogLine),
    /// A login flow waits for the user, such as entering a device code.
    AuthPrompt(AuthPrompt),
}

/// Final state of a step.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step ran and succeeded.
    Succeeded,
    /// The step had nothing to do, for example a file already present with the right hash.
    Skipped,
    /// The step failed.
    Failed {
        /// Human-readable reason.
        message: String,
    },
    /// The step stopped because the Operation was cancelled.
    Cancelled,
}

/// Final state of an Operation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationOutcome {
    /// Every step succeeded or was skipped, and the staged changes were moved into place.
    Succeeded,
    /// A step failed and the staged changes were rolled back.
    Failed {
        /// Human-readable reason.
        message: String,
    },
    /// The Operation was cancelled and the staged changes were rolled back.
    Cancelled,
}

/// Warning attached to an Operation or to the SDK as a whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// The Operation the warning belongs to, if any.
    pub op: Option<OperationId>,
    /// Stable machine-readable code.
    pub code: WarningCode,
    /// Human-readable description.
    pub message: String,
}

/// Stable, machine-readable warning code such as `catalog.offline-cache`.
///
/// UIs match on codes to show their own text; messages may change between releases.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct WarningCode(Cow<'static, str>);

impl WarningCode {
    /// Wraps a code written in the source.
    #[must_use]
    pub const fn from_static(code: &'static str) -> Self {
        Self(Cow::Borrowed(code))
    }

    /// Wraps a code built at run time, such as one defined by a third-party extension.
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self(Cow::Owned(code.into()))
    }

    /// Returns the code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WarningCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
