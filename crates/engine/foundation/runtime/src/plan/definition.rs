use std::fmt;
use std::marker::PhantomData;

use coblet_types::FileHash;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{PlanPath, Step, StepGraph};
use crate::{ExecutionReport, PlanId};

/// Kind of change a Plan describes, such as installing an Instance or importing a pack.
///
/// The kind fixes the preview type shown to users before anything runs and
/// the value an Operation produces when it finishes.
pub trait PlanKind: 'static {
    /// Name written into serialized Plans and checked when reading them back, such as `install`.
    const NAME: &'static str;

    /// Preview of the change: what is downloaded, how large it is, which files change,
    /// conflicts and warnings.
    type Summary: Serialize + DeserializeOwned + Clone + fmt::Debug + Send + Sync;

    /// Result of executing the Plan.
    type Output: Send + 'static;

    /// Builds the result once every step has finished.
    fn finish(summary: Self::Summary, report: &ExecutionReport) -> Self::Output;
}

/// Condition rechecked right before execution; a failed check stops the Plan as stale.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Precondition {
    /// The file still has the content it had when the Plan was made.
    FileUnchanged {
        /// The file.
        path: PlanPath,
        /// Its hash at planning time, or `None` if it did not exist.
        hash: Option<FileHash>,
    },
}

/// Complete description of a change, produced before any side effect happens.
///
/// A Plan can be previewed through [`Plan::preview`], serialized (unless it
/// contains a custom step) and handed to an Operation for execution.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{ExecutionReport, Plan, PlanKind, StepGraph};
///
/// struct Cleanup;
///
/// impl PlanKind for Cleanup {
///     const NAME: &'static str = "cleanup";
///     type Summary = Vec<String>;
///     type Output = ();
///     fn finish(_: Vec<String>, _: &ExecutionReport) {}
/// }
///
/// let plan: Plan<Cleanup> = Plan::new(vec!["remove logs/".to_owned()], StepGraph::new(), Vec::new());
/// let json = serde_json::to_string(&plan)?;
/// let back: Plan<Cleanup> = serde_json::from_str(&json)?;
/// assert_eq!(back.id(), plan.id());
/// assert_eq!(back.preview(), &["remove logs/".to_owned()]);
/// # Ok::<(), serde_json::Error>(())
/// ```
pub struct Plan<K: PlanKind> {
    id: PlanId,
    summary: K::Summary,
    steps: StepGraph,
    preconditions: Vec<Precondition>,
    kind: PhantomData<fn() -> K>,
}

impl<K: PlanKind> Plan<K> {
    /// Assembles a Plan with a fresh random ID.
    #[must_use]
    pub fn new(summary: K::Summary, steps: StepGraph, preconditions: Vec<Precondition>) -> Self {
        Self {
            id: PlanId::random(),
            summary,
            steps,
            preconditions,
            kind: PhantomData,
        }
    }

    /// Returns the Plan ID, which also names its staging directory and journal.
    #[must_use]
    pub fn id(&self) -> PlanId {
        self.id
    }

    /// Returns the preview shown to users before execution.
    #[must_use]
    pub fn preview(&self) -> &K::Summary {
        &self.summary
    }

    /// Returns the steps.
    #[must_use]
    pub fn steps(&self) -> &StepGraph {
        &self.steps
    }

    /// Returns the conditions rechecked before execution.
    #[must_use]
    pub fn preconditions(&self) -> &[Precondition] {
        &self.preconditions
    }

    /// Splits the Plan into its ID, summary, steps and preconditions.
    pub(crate) fn into_parts(self) -> (PlanId, K::Summary, StepGraph, Vec<Precondition>) {
        (self.id, self.summary, self.steps, self.preconditions)
    }

    /// Reports whether the Plan contains a custom step and therefore cannot be serialized.
    #[must_use]
    pub fn has_custom_steps(&self) -> bool {
        self.steps
            .iter()
            .any(|(_, node)| matches!(node.step(), Step::Custom(_)))
    }
}

impl<K: PlanKind> Clone for Plan<K> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            summary: self.summary.clone(),
            steps: self.steps.clone(),
            preconditions: self.preconditions.clone(),
            kind: PhantomData,
        }
    }
}

impl<K: PlanKind> fmt::Debug for Plan<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plan")
            .field("kind", &K::NAME)
            .field("id", &self.id)
            .field("summary", &self.summary)
            .field("steps", &self.steps)
            .field("preconditions", &self.preconditions)
            .finish()
    }
}

/// Serialized layout of a Plan, borrowed for writing.
#[derive(Serialize)]
struct WireRef<'a, S> {
    kind: &'static str,
    id: PlanId,
    summary: &'a S,
    steps: &'a StepGraph,
    #[serde(skip_serializing_if = "<[Precondition]>::is_empty")]
    preconditions: &'a [Precondition],
}

/// Serialized layout of a Plan, owned for reading.
#[derive(Deserialize)]
struct WireOwned<S> {
    kind: String,
    id: PlanId,
    summary: S,
    steps: StepGraph,
    #[serde(default)]
    preconditions: Vec<Precondition>,
}

impl<K: PlanKind> Serialize for Plan<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireRef {
            kind: K::NAME,
            id: self.id,
            summary: &self.summary,
            steps: &self.steps,
            preconditions: &self.preconditions,
        }
        .serialize(serializer)
    }
}

impl<'de, K: PlanKind> Deserialize<'de> for Plan<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireOwned::<K::Summary>::deserialize(deserializer)?;
        if wire.kind != K::NAME {
            return Err(D::Error::custom(format!(
                "plan kind is `{}`, expected `{}`",
                wire.kind,
                K::NAME
            )));
        }
        Ok(Self {
            id: wire.id,
            summary: wire.summary,
            steps: wire.steps,
            preconditions: wire.preconditions,
            kind: PhantomData,
        })
    }
}

#[cfg(test)]
#[path = "../../tests/plan/definition.rs"]
mod tests;
