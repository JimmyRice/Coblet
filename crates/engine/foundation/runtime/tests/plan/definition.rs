use std::sync::Arc;

use coblet_types::FileHash;

use super::{Plan, PlanKind, Precondition};
use crate::{
    BoxFuture, CustomStep, ExecutionReport, MetadataStep, PlanPath, RelPath, Step, StepContext,
    StepError, StepGraph, StepOutcome,
};

struct Rename;

impl PlanKind for Rename {
    const NAME: &'static str = "rename";
    type Summary = String;
    type Output = ();
    fn finish(_: String, _: &ExecutionReport) {}
}

struct Other;

impl PlanKind for Other {
    const NAME: &'static str = "other";
    type Summary = String;
    type Output = ();
    fn finish(_: String, _: &ExecutionReport) {}
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
        fn run<'a>(
            &'a self,
            _cx: &'a StepContext,
        ) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
            Box::pin(async { Ok(StepOutcome::Succeeded) })
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
