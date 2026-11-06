use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier of a Plan, stable across process restarts so
/// an interrupted Operation can be resumed from its journal.
///
/// # Examples
///
/// ```
/// use coblet_runtime::PlanId;
///
/// let id = PlanId::random();
/// assert_eq!(id.to_string().parse::<PlanId>()?, id);
/// # Ok::<(), coblet_runtime::IdError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlanId(Uuid);

/// Identifier of one running or finished Operation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(Uuid);

/// Reason an ID string was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The string is not a hyphenated UUID.
    #[error("`{0}` is not a uuid")]
    NotUuid(String),
}

impl PlanId {
    /// Generates a random (version 4) ID.
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl OperationId {
    /// Generates a random (version 4) ID.
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for PlanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl fmt::Debug for PlanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlanId({self})")
    }
}

impl FromStr for PlanId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::try_parse(s)
            .map(Self)
            .map_err(|_| IdError::NotUuid(s.to_owned()))
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl fmt::Debug for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OperationId({self})")
    }
}

impl FromStr for OperationId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::try_parse(s)
            .map(Self)
            .map_err(|_| IdError::NotUuid(s.to_owned()))
    }
}

/// Position of a step inside its Plan's step graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepId(pub(crate) u32);

impl StepId {
    /// Returns the index of the step in insertion order.
    #[must_use]
    pub fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "step {}", self.0)
    }
}

#[cfg(test)]
#[path = "../tests/ids.rs"]
mod tests;
