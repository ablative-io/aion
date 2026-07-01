//! Server-side intervention routing tests: the capability gate + owner resolution.
//!
//! These use a RECORDING in-proc [`InterventionTransport`] so the negative control
//! is exact: a command the server gates on capabilities must NEVER reach the
//! transport. The applied path routes a real command through the router to the
//! transport and back as a neutral ack.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aion_core::{
    ActivityId, InjectPriority, InterventionCapabilities, InterventionCommand, InterventionKind,
    InterventionOutcome, InterventionPrimitive, WorkflowId,
};
use async_trait::async_trait;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{AttemptKey, AttemptOwnerIndex, InterventionRouter, InterventionTransport};
use crate::error::ServerError;
use crate::worker::registry::{
    ConnectedWorkerRegistry, WorkerDelivery, WorkerHandle, WorkerId, WorkerRegistration,
};

/// A transport that records every command it is asked to push and answers with a
/// fixed outcome — so a test can assert a gated command NEVER reaches it.
#[derive(Default)]
struct RecordingTransport {
    pushed: AtomicUsize,
}

#[async_trait]
impl InterventionTransport for RecordingTransport {
    async fn push(
        &self,
        _worker: &WorkerHandle,
        _command: InterventionCommand,
    ) -> Result<InterventionOutcome, ServerError> {
        self.pushed.fetch_add(1, Ordering::SeqCst);
        Ok(InterventionOutcome::Applied)
    }
}

/// A transport that always reports the connection was lost — to prove the router
/// maps a transport fault onto the stale-target no-op, never a crash.
struct ConnectionLostTransport;

#[async_trait]
impl InterventionTransport for ConnectionLostTransport {
    async fn push(
        &self,
        _worker: &WorkerHandle,
        _command: InterventionCommand,
    ) -> Result<InterventionOutcome, ServerError> {
        Err(ServerError::worker_connection_lost(
            "liminal-push",
            "worker gone",
        ))
    }
}

fn command(attempt: u32, kind: InterventionKind) -> InterventionCommand {
    InterventionCommand {
        workflow_id: WorkflowId::new(Uuid::nil()),
        activity_id: ActivityId::from_sequence_position(3),
        attempt,
        issued_by: Some("operator".to_owned()),
        issued_at: chrono::Utc::now(),
        kind,
    }
}

fn inject(attempt: u32) -> InterventionCommand {
    command(
        attempt,
        InterventionKind::InjectMessage {
            text: "steer".to_owned(),
            priority: InjectPriority::Interrupt,
        },
    )
}

fn key(attempt: u32) -> AttemptKey {
    AttemptKey::new(
        WorkflowId::new(Uuid::nil()),
        ActivityId::from_sequence_position(3),
        attempt,
    )
}

/// Register a worker advertising `capabilities`, returning the registry, its id,
/// and the held registration guard (kept alive so the entry stays registered).
fn register_worker(
    registry: &ConnectedWorkerRegistry,
    capabilities: InterventionCapabilities,
) -> (WorkerId, WorkerRegistration) {
    let (tx, _rx) = mpsc::channel(1);
    let types = [String::from("agent")];
    let registration = registry
        .register_delivery_with_capabilities(
            [String::from("default")],
            String::from("default"),
            None,
            types.iter(),
            WorkerDelivery::Grpc(tx),
            capabilities,
        )
        .expect("registration succeeds");
    let id = registration.worker_id().expect("assigned an id");
    (id, registration)
}

fn caps_inject_cancel() -> InterventionCapabilities {
    InterventionCapabilities::from_primitives([
        InterventionPrimitive::InjectMessage,
        InterventionPrimitive::Cancel,
    ])
}

#[tokio::test]
async fn routes_an_advertised_command_to_the_owning_worker() {
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, _guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    owners.bind(key(1), worker_id);
    let transport = Arc::new(RecordingTransport::default());
    let router = InterventionRouter::new(registry, owners, Arc::clone(&transport) as Arc<_>);

    let outcome = router.route(inject(1)).await.expect("route succeeds");
    assert_eq!(outcome, InterventionOutcome::Applied);
    assert_eq!(
        transport.pushed.load(Ordering::SeqCst),
        1,
        "an advertised command is pushed to the worker"
    );
}

#[tokio::test]
async fn an_unadvertised_primitive_is_gated_at_the_server_and_never_sent() {
    // The owner advertises only {InjectMessage, Cancel}; a PauseResume command must
    // be refused at the SERVER and NEVER reach the transport (negative control a).
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, _guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    owners.bind(key(1), worker_id);
    let transport = Arc::new(RecordingTransport::default());
    let router = InterventionRouter::new(registry, owners, Arc::clone(&transport) as Arc<_>);

    let gated = command(1, InterventionKind::PauseResume { paused: true });
    let outcome = router
        .route(gated)
        .await
        .expect("route returns a gated ack");
    assert!(matches!(
        outcome,
        InterventionOutcome::CapabilityNotSupported {
            primitive: InterventionPrimitive::PauseResume
        }
    ));
    assert_eq!(
        transport.pushed.load(Ordering::SeqCst),
        0,
        "a gated command must NEVER be pushed to the worker"
    );
}

#[tokio::test]
async fn a_command_for_an_unowned_attempt_is_a_stale_target_no_op() {
    // The owner index binds attempt 1; a command for attempt 2 (never ran / already
    // finished) has no owner and is the too-late no-op — an app-range NACK, never a
    // panic (negative control b).
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, _guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    owners.bind(key(1), worker_id);
    let transport = Arc::new(RecordingTransport::default());
    let router = InterventionRouter::new(registry, owners, Arc::clone(&transport) as Arc<_>);

    let outcome = router.route(inject(2)).await.expect("route returns a NACK");
    assert!(matches!(outcome, InterventionOutcome::StaleTarget { .. }));
    assert_eq!(transport.pushed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_disconnected_owner_is_a_stale_target_no_op() {
    // The owner id is bound but the worker has since deregistered (it disconnected):
    // the router resolves no live handle and returns the too-late no-op.
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    owners.bind(key(1), worker_id);
    // The worker disconnects, but the owner index still points at its id.
    guard.deregister().expect("deregister succeeds");
    let transport = Arc::new(RecordingTransport::default());
    let router = InterventionRouter::new(registry, owners, Arc::clone(&transport) as Arc<_>);

    let outcome = router.route(inject(1)).await.expect("route returns a NACK");
    assert!(matches!(outcome, InterventionOutcome::StaleTarget { .. }));
    assert_eq!(transport.pushed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_transport_connection_loss_maps_to_a_stale_target_no_op() {
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, _guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    owners.bind(key(1), worker_id);
    let router = InterventionRouter::new(registry, owners, Arc::new(ConnectionLostTransport));

    let outcome = router.route(inject(1)).await.expect("route returns a NACK");
    assert!(matches!(outcome, InterventionOutcome::StaleTarget { .. }));
}

#[tokio::test]
async fn capabilities_for_reads_the_owning_workers_advertised_set() {
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, _guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    owners.bind(key(1), worker_id);
    let router = InterventionRouter::new(registry, owners, Arc::new(RecordingTransport::default()));

    let caps = router
        .capabilities_for(&key(1))
        .expect("lookup succeeds")
        .expect("an owner is bound");
    assert!(caps.supports_primitive(InterventionPrimitive::InjectMessage));
    assert!(!caps.supports_primitive(InterventionPrimitive::PauseResume));

    // No owner for attempt 2 => no capabilities.
    assert!(router.capabilities_for(&key(2)).expect("lookup").is_none());
}

#[tokio::test]
async fn intervenable_attempts_enumerates_only_live_owned_attempts_of_the_workflow() {
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, _guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    // Two live attempts of THIS workflow's activity, plus one attempt of a
    // DIFFERENT workflow that must not leak into the enumeration.
    let this_workflow = WorkflowId::new(Uuid::nil());
    let other_workflow = WorkflowId::new(Uuid::from_u128(7));
    owners.bind(key(1), worker_id);
    owners.bind(key(2), worker_id);
    owners.bind(
        AttemptKey::new(other_workflow, ActivityId::from_sequence_position(3), 1),
        worker_id,
    );
    let router = InterventionRouter::new(registry, owners, Arc::new(RecordingTransport::default()));

    let mut attempts = router
        .intervenable_attempts(&this_workflow)
        .expect("enumeration succeeds");
    attempts.sort_by_key(|(attempt_key, _caps)| attempt_key.attempt);
    assert_eq!(attempts.len(), 2, "only this workflow's live attempts appear");
    assert_eq!(attempts[0].0.attempt, 1);
    assert_eq!(attempts[1].0.attempt, 2);
    // Each carries the SAME advertised set the router gates on.
    for (_key, caps) in &attempts {
        assert!(caps.supports_primitive(InterventionPrimitive::InjectMessage));
        assert!(!caps.supports_primitive(InterventionPrimitive::PauseResume));
    }
}

#[tokio::test]
async fn intervenable_attempts_drops_an_attempt_whose_owner_disconnected() {
    // The attempt is bound in the owner index but its worker has since
    // deregistered: the enumeration must omit it (no control for an unreachable
    // attempt), never surface a phantom entry.
    let registry = ConnectedWorkerRegistry::default();
    let (worker_id, guard) = register_worker(&registry, caps_inject_cancel());
    let owners = AttemptOwnerIndex::new();
    owners.bind(key(1), worker_id);
    guard.deregister().expect("deregister succeeds");
    let router = InterventionRouter::new(registry, owners, Arc::new(RecordingTransport::default()));

    let attempts = router
        .intervenable_attempts(&WorkflowId::new(Uuid::nil()))
        .expect("enumeration succeeds");
    assert!(
        attempts.is_empty(),
        "a disconnected owner's attempt must not be enumerated"
    );
}
