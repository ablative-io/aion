//! Worker-side intervention delivery tests: the attempt back-index gate + no-op.
//!
//! These drive a REAL `spawn_agent` driver over an in-crate fake session (the same
//! neutral seam `agent_tests` uses) so the full worker apply path — back-index
//! lookup, capability gate, control-channel delivery, driver ack — is exercised,
//! not a stub.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::Mutex;

use aion_core::{
    ActivityEvent, ActivityId, ContentType, InjectPriority, InterventionCapabilities,
    InterventionCommand, InterventionKind, InterventionOutcome, InterventionPrimitive, Payload,
    WorkflowId,
};
use aion_integrations::contract::{AgentHarness, AgentSession};
use aion_integrations::error::HarnessError;
use aion_integrations::spec::AgentRunSpec;
use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{ControlRegistry, SessionKey};
use crate::runtime::agent::spawn_agent;

/// A fake session that records applied interventions and gates the rest. Its event
/// stream stays open (draining a channel) until the caller closes `end_events`, so
/// the session stays live while a command is delivered and then ends cleanly.
struct FakeSession {
    capabilities: InterventionCapabilities,
    applied: Arc<Mutex<Vec<InterventionKind>>>,
    end_events: Option<mpsc::UnboundedReceiver<ActivityEvent>>,
}

struct FakeHarness {
    session: Mutex<Option<FakeSession>>,
}

#[async_trait]
impl AgentHarness for FakeHarness {
    type Session = FakeSession;

    async fn start(&self, _spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        self.session
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| HarnessError::transport("started twice"))
    }
}

#[async_trait]
impl AgentSession for FakeSession {
    fn capabilities(&self) -> &InterventionCapabilities {
        &self.capabilities
    }

    fn events(&mut self) -> BoxStream<'static, ActivityEvent> {
        // Drain a channel: the stream stays open (run stays live) until the caller
        // closes the sender, at which point the stream ends and the run finishes.
        match self.end_events.take() {
            Some(receiver) => Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(
                receiver,
            )),
            None => Box::pin(futures::stream::empty()),
        }
    }

    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError> {
        if !self.capabilities.supports(&cmd.kind) {
            return Err(HarnessError::capability_not_supported("gated"));
        }
        self.applied.lock().unwrap().push(cmd.kind);
        Ok(())
    }

    async fn wait_result(self) -> Result<Payload, HarnessError> {
        Ok(Payload::new(ContentType::Json, b"null".to_vec()))
    }
}

fn key(attempt: u32) -> SessionKey {
    SessionKey::new(
        WorkflowId::new(Uuid::nil()),
        ActivityId::from_sequence_position(3),
        attempt,
    )
}

fn command(attempt: u32, kind: InterventionKind) -> InterventionCommand {
    InterventionCommand {
        workflow_id: WorkflowId::new(Uuid::nil()),
        activity_id: ActivityId::from_sequence_position(3),
        attempt,
        issued_by: Some("operator".to_owned()),
        issued_at: Utc::now(),
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

fn caps_inject() -> InterventionCapabilities {
    InterventionCapabilities::from_primitives([InterventionPrimitive::InjectMessage])
}

/// A live fake session driven by a real `spawn_agent`. Holding `end_events` keeps
/// the run alive; dropping it ends the run so the driver task can join.
struct LiveSession {
    applied: Arc<Mutex<Vec<InterventionKind>>>,
    guard: super::SessionGuard,
    end_events: mpsc::UnboundedSender<ActivityEvent>,
    driver: tokio::task::JoinHandle<()>,
}

impl LiveSession {
    /// End the run and join the driver task.
    async fn shutdown(self) {
        drop(self.end_events);
        drop(self.guard);
        let _ = self.driver.await;
    }
}

/// Register a live fake session under `attempt`, driving it with a real
/// `spawn_agent`.
fn live_session(
    registry: &ControlRegistry,
    attempt: u32,
    capabilities: InterventionCapabilities,
) -> LiveSession {
    let applied = Arc::new(Mutex::new(Vec::new()));
    let (end_events, events_rx) = mpsc::unbounded_channel();
    let session = FakeSession {
        capabilities: capabilities.clone(),
        applied: Arc::clone(&applied),
        end_events: Some(events_rx),
    };
    let harness = FakeHarness {
        session: Mutex::new(Some(session)),
    };
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let guard = registry.register(key(attempt), control_tx, capabilities);
    let driver = tokio::spawn(async move {
        let _ = spawn_agent(&harness, spec(), event_tx, Some(control_rx)).await;
    });
    LiveSession {
        applied,
        guard,
        end_events,
        driver,
    }
}

fn spec() -> AgentRunSpec {
    AgentRunSpec::new(
        WorkflowId::new(Uuid::nil()),
        ActivityId::from_sequence_position(3),
        1,
        Payload::new(ContentType::Json, b"\"in\"".to_vec()),
    )
}

#[tokio::test]
async fn delivers_an_advertised_command_to_the_live_session() {
    let registry = ControlRegistry::new();
    let session = live_session(&registry, 1, caps_inject());

    let outcome = registry.deliver(inject(1)).await;
    assert_eq!(outcome, InterventionOutcome::Applied);
    assert_eq!(session.applied.lock().unwrap().len(), 1);

    session.shutdown().await;
}

#[tokio::test]
async fn gates_an_unadvertised_primitive_at_the_worker() {
    // Session advertises only InjectMessage; a PauseResume command is gated by the
    // worker back-index WITHOUT reaching the session.
    let registry = ControlRegistry::new();
    let session = live_session(&registry, 1, caps_inject());

    let gated = command(1, InterventionKind::PauseResume { paused: true });
    let outcome = registry.deliver(gated).await;
    assert!(matches!(
        outcome,
        InterventionOutcome::CapabilityNotSupported { .. }
    ));
    assert!(
        session.applied.lock().unwrap().is_empty(),
        "a gated command must not reach the session"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn a_command_for_an_unknown_attempt_is_a_stale_target_no_op() {
    // A live session at attempt 1; a command for attempt 2 (never ran here) is the
    // attempt-scoped no-op — a stale-target NACK, not a panic.
    let registry = ControlRegistry::new();
    let session = live_session(&registry, 1, caps_inject());

    let outcome = registry.deliver(inject(2)).await;
    assert!(matches!(outcome, InterventionOutcome::StaleTarget { .. }));

    session.shutdown().await;
}

#[tokio::test]
async fn a_command_to_an_empty_registry_is_a_stale_target_no_op() {
    let registry = ControlRegistry::new();
    let outcome = registry.deliver(inject(1)).await;
    assert!(matches!(outcome, InterventionOutcome::StaleTarget { .. }));
}

#[tokio::test]
async fn a_deregistered_session_no_longer_receives_commands() {
    let registry = ControlRegistry::new();
    let session = live_session(&registry, 1, caps_inject());
    session.shutdown().await;

    // After the guard drops, the back-index has no entry: the command is a no-op.
    let outcome = registry.deliver(inject(1)).await;
    assert!(matches!(outcome, InterventionOutcome::StaleTarget { .. }));
}
