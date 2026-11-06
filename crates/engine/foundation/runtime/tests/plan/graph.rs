use std::collections::HashSet;

use super::{StepGraph, StepGraphError};
use crate::{PlanPath, RelPath, RemoveMode, RemoveStep, Step, StepId};

fn remove(name: &str) -> Step {
    Step::Remove(RemoveStep {
        path: PlanPath::target(RelPath::new(name).unwrap()),
        mode: RemoveMode::Delete,
    })
}

fn diamond() -> (StepGraph, [StepId; 4]) {
    let mut graph = StepGraph::new();
    let a = graph.add("a", remove("a"), &[]).unwrap();
    let b = graph.add("b", remove("b"), &[a]).unwrap();
    let c = graph.add("c", remove("c"), &[a, a]).unwrap();
    let d = graph.add("d", remove("d"), &[c, b]).unwrap();
    (graph, [a, b, c, d])
}

#[test]
fn rejects_forward_and_self_references() {
    let mut graph = StepGraph::new();
    let first = graph.add("first", remove("x"), &[]).unwrap();
    let err = graph.add("second", remove("y"), &[StepId(1)]).unwrap_err();
    assert_eq!(
        err,
        StepGraphError::UnknownDependency {
            step: StepId(1),
            dependency: StepId(1)
        }
    );
    assert_eq!(graph.len(), 1);
    assert_eq!(graph.get(first).unwrap().label(), "first");
}

#[test]
fn walks_a_diamond() {
    let (graph, [a, b, c, d]) = diamond();
    assert_eq!(graph.get(c).unwrap().after(), [a]);
    assert_eq!(graph.get(d).unwrap().after(), [b, c]);
    assert_eq!(graph.ready(&HashSet::new()), [a]);
    assert_eq!(graph.ready(&HashSet::from([a])), [b, c]);
    assert_eq!(graph.ready(&HashSet::from([a, b])), [c]);
    assert_eq!(graph.ready(&HashSet::from([a, b, c])), [d]);
    assert_eq!(graph.ready(&HashSet::from([a, b, c, d])), []);
    assert_eq!(graph.dependents(a), [b, c]);
}

#[test]
fn deserialization_rejects_cycles() {
    let (graph, _) = diamond();
    let json = serde_json::to_string(&graph).unwrap();
    assert_eq!(serde_json::from_str::<StepGraph>(&json).unwrap(), graph);

    let cyclic = json.replacen(r#""after":[0]"#, r#""after":[3]"#, 1);
    assert_ne!(cyclic, json);
    assert!(serde_json::from_str::<StepGraph>(&cyclic).is_err());
}
