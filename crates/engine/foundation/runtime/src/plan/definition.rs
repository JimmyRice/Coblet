use std::fmt;
use std::marker::PhantomData;

use coblet_types::FileHash;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{PlanPath, Step, StepGraph};
use crate::PlanId;

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
    type Output: Send;
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
/// use coblet_runtime::{Plan, PlanKind, StepGraph};
///
/// struct Cleanup;
///
/// impl PlanKind for Cleanup {
///     const NAME: &'static str = "cleanup";
///     type Summary = Vec<String>;
///     type Output = ();
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
mod tests {
    use std::sync::Arc;

    use coblet_types::FileHash;

    use super::{Plan, PlanKind, Precondition};
    use crate::{CustomStep, MetadataStep, PlanPath, RelPath, Step, StepGraph};

    struct Rename;

    impl PlanKind for Rename {
        const NAME: &'static str = "rename";
        type Summary = String;
        type Output = ();
    }

    struct Other;

    impl PlanKind for Other {
        const NAME: &'static str = "other";
        type Summary = String;
        type Output = ();
    }

    fn sample() -> Plan<Rename> {
        let cfg = PlanPath::target(RelPath::new("instance.cfg").unwrap());
        let mut steps = StepGraph::new();
        steps
            .add(
                "write instance.cfg",
                Step::WriteMetadata(MetadataStep {
                    path: cfg.clone(),
                    contents: "name=New\n".to_owned(),
                }),
                &[],
            )
            .unwrap();
        let hash = FileHash::new("sha256", &"0".repeat(64)).unwrap();
        Plan::new(
            "Old -> New".to_owned(),
            steps,
            vec![Precondition::FileUnchanged {
                path: cfg,
                hash: Some(hash),
            }],
        )
    }

    #[test]
    fn round_trips_and_checks_the_kind() {
        let plan = sample();
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["kind"], "rename");
        assert_eq!(json["preconditions"][0]["type"], "file_unchanged");

        let back: Plan<Rename> = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.id(), plan.id());
        assert_eq!(back.preview(), "Old -> New");
        assert_eq!(back.steps(), plan.steps());
        assert_eq!(back.preconditions(), plan.preconditions());

        let error = serde_json::from_value::<Plan<Other>>(json).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("plan kind is `rename`, expected `other`")
        );
    }

    #[test]
    fn plans_with_custom_steps_do_not_serialize() {
        #[derive(Debug)]
        struct Noop;
        impl CustomStep for Noop {
            fn kind(&self) -> &'static str {
                "acme:noop"
            }
            fn describe(&self) -> String {
                String::new()
            }
        }

        let mut steps = StepGraph::new();
        steps
            .add("noop", Step::Custom(Arc::new(Noop)), &[])
            .unwrap();
        let plan: Plan<Rename> = Plan::new(String::new(), steps, Vec::new());
        assert!(plan.has_custom_steps());
        assert!(!sample().has_custom_steps());
        assert!(serde_json::to_string(&plan).is_err());
        assert_ne!(plan.clone().id(), sample().id());
    }
}
