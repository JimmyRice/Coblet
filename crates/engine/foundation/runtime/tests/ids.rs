use super::{IdError, OperationId, PlanId};

#[test]
fn generates_distinct_ids() {
    assert_ne!(PlanId::random(), PlanId::random());
    assert_ne!(OperationId::random(), OperationId::random());
}

#[test]
fn round_trips_text_and_json() {
    let id = PlanId::random();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, format!("\"{id}\""));
    assert_eq!(serde_json::from_str::<PlanId>(&json).unwrap(), id);
    assert_eq!(
        "nope".parse::<OperationId>(),
        Err(IdError::NotUuid("nope".to_owned()))
    );
}
