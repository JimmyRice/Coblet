//! Compile-time checks that public types stay usable across threads.

use crate::{
    CancellationToken, Event, EventBus, EventSubscription, Extension, HttpUrl, MemorySecretStore,
    OperationId, PlanId, Precondition, Registry, Request, Response, Secret, SecretKey, Step,
    StepGraph, UserAgent,
};

const fn assert_send_sync<T: Send + Sync + ?Sized>() {}

const fn assert_send<T: Send + ?Sized>() {}

const _: () = {
    assert_send_sync::<CancellationToken>();
    assert_send_sync::<Event>();
    assert_send_sync::<EventBus>();
    assert_send_sync::<HttpUrl>();
    assert_send_sync::<MemorySecretStore>();
    assert_send_sync::<OperationId>();
    assert_send_sync::<PlanId>();
    assert_send_sync::<Precondition>();
    assert_send_sync::<Registry<dyn Extension>>();
    assert_send_sync::<Request>();
    assert_send_sync::<Secret>();
    assert_send_sync::<SecretKey>();
    assert_send_sync::<Step>();
    assert_send_sync::<StepGraph>();
    assert_send_sync::<UserAgent>();
    // Bodies are read by one task at a time, so a response only needs to move between threads.
    assert_send::<Response>();
    assert_send::<EventSubscription>();
};
