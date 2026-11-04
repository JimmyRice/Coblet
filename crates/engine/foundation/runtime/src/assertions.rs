//! Compile-time checks that public types stay usable across threads.

use crate::{
    CancellationToken, ComputedHashes, Event, EventBus, EventSubscription, ExecutionReport,
    Extension, HttpUrl, KillHandle, MemorySecretStore, OperationError, OperationHandle,
    OperationId, OperationProgress, Operations, PlanId, Precondition, ProcessSpec, Registry,
    Request, Response, Secret, SecretKey, SpawnedProcess, Step, StepContext, StepError, StepGraph,
    StreamHasher, UserAgent,
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
    assert_send_sync::<ComputedHashes>();
    assert_send_sync::<ExecutionReport>();
    assert_send_sync::<KillHandle>();
    assert_send_sync::<OperationError>();
    assert_send_sync::<OperationProgress>();
    assert_send_sync::<Operations>();
    assert_send_sync::<ProcessSpec>();
    assert_send_sync::<StepContext>();
    assert_send_sync::<StepError>();
    assert_send_sync::<StreamHasher>();
    #[cfg(feature = "fs")]
    assert_send_sync::<crate::TokioStorage>();
    #[cfg(feature = "process")]
    assert_send_sync::<crate::TokioSpawner>();
    #[cfg(feature = "reqwest")]
    assert_send_sync::<crate::ReqwestTransport>();
    assert_send::<OperationHandle<()>>();
    assert_send::<SpawnedProcess>();
    // Bodies are read by one task at a time, so a response only needs to move between threads.
    assert_send::<Response>();
    assert_send::<EventSubscription>();
};
