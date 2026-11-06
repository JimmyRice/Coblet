use std::collections::HashSet;

use serde::{Deserialize, Serialize, Serializer};

use super::Step;
use crate::StepId;

/// Directed acyclic graph of a Plan's steps.
///
/// A step can only depend on steps added before it, so the graph has no
/// cycles by construction and insertion order is a valid execution order.
/// Deserialization applies the same rule.
///
/// # Examples
///
/// ```
/// use std::collections::HashSet;
///
/// use coblet_runtime::{MoveStep, PlanPath, RelPath, RemoveMode, RemoveStep, Step, StepGraph};
///
/// let staged = PlanPath::staging(RelPath::new("mods/a.jar")?);
/// let target = PlanPath::target(RelPath::new("mods/a.jar")?);
/// let mut graph = StepGraph::new();
/// let remove = graph.add(
///     "remove old jar",
///     Step::Remove(RemoveStep { path: target.clone(), mode: RemoveMode::Trash }),
///     &[],
/// )?;
/// let place = graph.add("place jar", Step::MoveIntoPlace(MoveStep { from: staged, to: target }), &[remove])?;
///
/// assert_eq!(graph.ready(&HashSet::new()), [remove]);
/// assert_eq!(graph.ready(&HashSet::from([remove])), [place]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(try_from = "Vec<StepNode>")]
pub struct StepGraph {
    nodes: Vec<StepNode>,
}

/// Step together with its label and the steps it waits for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepNode {
    label: String,
    step: Step,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    after: Vec<StepId>,
}

/// Reason a step could not be added to a [`StepGraph`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StepGraphError {
    /// A dependency names a step that is not (yet) in the graph.
    #[error("{dependency} is not in the graph before {step}")]
    UnknownDependency {
        /// The step being added.
        step: StepId,
        /// The missing dependency.
        dependency: StepId,
    },
    /// The graph already holds `u32::MAX` steps.
    #[error("a plan cannot hold more than u32::MAX steps")]
    TooManySteps,
}

impl StepNode {
    /// Returns the label shown in previews and progress, such as `download client.jar`.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the step.
    #[must_use]
    pub fn step(&self) -> &Step {
        &self.step
    }

    /// Returns the steps that must finish first.
    #[must_use]
    pub fn after(&self) -> &[StepId] {
        &self.after
    }
}

impl StepGraph {
    /// Creates an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a step that runs once every step in `after` has finished.
    ///
    /// # Errors
    ///
    /// Returns [`StepGraphError::UnknownDependency`] when `after` names a step
    /// not yet in the graph, or [`StepGraphError::TooManySteps`].
    pub fn add(
        &mut self,
        label: impl Into<String>,
        step: Step,
        after: &[StepId],
    ) -> Result<StepId, StepGraphError> {
        let id = StepId(u32::try_from(self.nodes.len()).map_err(|_| StepGraphError::TooManySteps)?);
        if let Some(&dependency) = after.iter().find(|dep| dep.0 >= id.0) {
            return Err(StepGraphError::UnknownDependency {
                step: id,
                dependency,
            });
        }
        let mut after = after.to_vec();
        after.sort_unstable();
        after.dedup();
        self.nodes.push(StepNode {
            label: label.into(),
            step,
            after,
        });
        Ok(id)
    }

    /// Looks up a step.
    #[must_use]
    pub fn get(&self, id: StepId) -> Option<&StepNode> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.nodes.get(index))
    }

    /// Iterates over steps in a valid execution order.
    pub fn iter(&self) -> impl Iterator<Item = (StepId, &StepNode)> {
        // Indices fit in u32 because `add` refuses to grow past u32::MAX.
        (0_u32..)
            .zip(&self.nodes)
            .map(|(index, node)| (StepId(index), node))
    }

    /// Returns the number of steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Reports whether the graph has no steps.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Lists unfinished steps whose dependencies are all in `finished`, in execution order.
    #[must_use]
    pub fn ready(&self, finished: &HashSet<StepId>) -> Vec<StepId> {
        self.iter()
            .filter(|(id, node)| {
                !finished.contains(id) && node.after.iter().all(|dep| finished.contains(dep))
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Lists the steps that directly wait for `id`.
    #[must_use]
    pub fn dependents(&self, id: StepId) -> Vec<StepId> {
        self.iter()
            .filter(|(_, node)| node.after.contains(&id))
            .map(|(dependent, _)| dependent)
            .collect()
    }
}

impl TryFrom<Vec<StepNode>> for StepGraph {
    type Error = StepGraphError;

    fn try_from(nodes: Vec<StepNode>) -> Result<Self, Self::Error> {
        let mut graph = Self {
            nodes: Vec::with_capacity(nodes.len()),
        };
        for node in nodes {
            graph.add(node.label, node.step, &node.after)?;
        }
        Ok(graph)
    }
}

impl Serialize for StepGraph {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.nodes.serialize(serializer)
    }
}

#[cfg(test)]
#[path = "../../tests/plan/graph.rs"]
mod tests;
