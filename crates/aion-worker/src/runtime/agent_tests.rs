//! Trait-driver tests against an IN-CRATE fake harness — NO norn, NO concrete
//! adapter. The fake implements the neutral `aion-integrations` seam directly, so
//! the driver's demux (events out, commands in, terminal result) is exercised
//! end-to-end while proving the driver is harness-blind: it names only the trait.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::Mutex;

use aion_core::{
    ActivityEvent, ActivityEventKind, ActivityId, ContentType, InterventionCapabilities,
    InterventionCommand, InterventionKind, InterventionPrimitive, MessageRole, Payload, WorkflowId,
};
use aion_integrations::contract::{AgentHarness, AgentSession};
use aion_integrations::error::HarnessError;
use aion_integrations::spec::AgentRunSpec;
use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use super::{ControlReceiver, harness_error_to_outcome, spawn_agent};
use crate::runtime::loop_::DispatchOutcome;

/// A fake session: yields a fixed batch of events, records every accepted
/// intervention, capability-gates the rest, and returns a canned result.
struct FakeSession {
    capabilities: InterventionCapabilities,
    events: Option<Vec<ActivityEvent>>,
    interventions: Arc<Mutex<Vec<InterventionKind>>>,
    result: Payload,
    /// When set, `wait_result` fails with this harness error instead of a payload.
    fail_result: Option<HarnessError>,
}

/// A fake harness that hands out one preconfigured [`FakeSession`].
struct FakeHarness {
    session: Mutex<Option<FakeSession>>,
}

impl FakeHarness {
    fn new(session: FakeSession) -> Self {
        Self {
            session: Mutex::new(Some(session)),
        }
    }
}

#[async_trait]
impl AgentHarness for FakeHarness {
    type Session = FakeSession;

    async fn start(&self, _spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        self.session
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| HarnessError::transport("fake harness started twice"))
    }
}

#[async_trait]
impl AgentSession for FakeSession {
    fn capabilities(&self) -> &InterventionCapabilities {
        &self.capabilities
    }

    fn events(&mut self) -> BoxStream<'static, ActivityEvent> {
        let batch = self.events.take().unwrap_or_default();
        Box::pin(futures::stream::iter(batch))
    }

    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError> {
        if !self.capabilities.supports(&cmd.kind) {
            return Err(HarnessError::capability_not_supported(format!(
                "{:?}",
                cmd.kind.primitive()
            )));
        }
        self.interventions.lock().unwrap().push(cmd.kind);
        Ok(())
    }

    async fn wait_result(self) -> Result<Payload, HarnessError> {
        match self.fail_result {
            Some(error) => Err(error),
            None => Ok(self.result),
        }
    }
}

fn message_event(text: &str) -> ActivityEvent {
    ActivityEvent {
        workflow_id: WorkflowId::new(uuid::Uuid::nil()),
        activity_id: ActivityId::from_sequence_position(2),
        attempt: 1,
        agent_id: uuid::Uuid::nil(),
        agent_role: "root".to_owned(),
        emitted_at: Utc::now(),
        worker_seq: 0,
        store_seq: None,
        ephemeral: false,
        kind: ActivityEventKind::Message {
            role: MessageRole::Assistant,
            text: text.to_owned(),
        },
    }
}

fn spec() -> AgentRunSpec {
    AgentRunSpec::new(
        WorkflowId::new(uuid::Uuid::nil()),
        ActivityId::from_sequence_position(2),
        1,
        Payload::new(ContentType::Json, b"\"in\"".to_vec()),
    )
}

fn inject_command() -> InterventionCommand {
    InterventionCommand {
        workflow_id: WorkflowId::new(uuid::Uuid::nil()),
        activity_id: ActivityId::from_sequence_position(2),
        attempt: 1,
        issued_by: Some("operator".to_owned()),
        issued_at: Utc::now(),
        kind: InterventionKind::InjectMessage {
            text: "steer".to_owned(),
            priority: aion_core::InjectPriority::Interrupt,
        },
    }
}

fn caps_inject_cancel() -> InterventionCapabilities {
    InterventionCapabilities::from_primitives([
        InterventionPrimitive::InjectMessage,
        InterventionPrimitive::Cancel,
    ])
}

#[tokio::test]
async fn drives_events_to_sink_and_captures_the_terminal_result() {
    let session = FakeSession {
        capabilities: caps_inject_cancel(),
        events: Some(vec![message_event("working"), message_event("done")]),
        interventions: Arc::new(Mutex::new(Vec::new())),
        result: Payload::new(ContentType::Json, b"{\"ok\":true}".to_vec()),
        fail_result: None,
    };
    let harness = FakeHarness::new(session);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    let outcome = spawn_agent(&harness, spec(), event_tx, None)
        .await
        .expect("driver runs to a terminal result");

    // Both events reached the sink, in order.
    let first = event_rx.recv().await.expect("first event forwarded");
    let second = event_rx.recv().await.expect("second event forwarded");
    assert!(matches!(
        first.kind,
        ActivityEventKind::Message { ref text, .. } if text == "working"
    ));
    assert!(matches!(
        second.kind,
        ActivityEventKind::Message { ref text, .. } if text == "done"
    ));
    assert!(event_rx.recv().await.is_none(), "sink closes after run");

    // The terminal result is the id-matched output, as DispatchOutcome::Completed.
    match outcome {
        DispatchOutcome::Completed { output } => {
            assert_eq!(output.content_type(), &ContentType::Json);
            assert_eq!(output.bytes(), b"{\"ok\":true}");
        }
        DispatchOutcome::Failed { failure } => panic!("expected completion, got {failure:?}"),
    }
}

#[tokio::test]
async fn feeds_control_commands_into_intervene() {
    let interventions = Arc::new(Mutex::new(Vec::new()));
    // No events, so the stream is empty and the run ends immediately AFTER the
    // control command is drained (biased select drains control first).
    let session = FakeSession {
        capabilities: caps_inject_cancel(),
        events: Some(Vec::new()),
        interventions: Arc::clone(&interventions),
        result: Payload::new(ContentType::Json, b"null".to_vec()),
        fail_result: None,
    };
    let harness = FakeHarness::new(session);
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx): (_, ControlReceiver) = mpsc::unbounded_channel();

    // Queue a command, then close the control channel so the loop can proceed to
    // draining the (empty) event stream and taking the result.
    control_tx.send(inject_command()).unwrap();
    drop(control_tx);

    let outcome = spawn_agent(&harness, spec(), event_tx, Some(control_rx))
        .await
        .expect("driver runs to a terminal result");

    let recorded = interventions.lock().unwrap();
    assert_eq!(recorded.len(), 1, "the queued command reached intervene");
    assert!(matches!(
        recorded[0],
        InterventionKind::InjectMessage { .. }
    ));
    assert!(matches!(outcome, DispatchOutcome::Completed { .. }));
}

#[tokio::test]
async fn a_capability_gated_command_is_not_fatal() {
    // The session advertises only InjectMessage; a PauseResume command is gated by
    // the session and rejected — the driver logs it and still runs to a result.
    let session = FakeSession {
        capabilities: InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
        ]),
        events: Some(Vec::new()),
        interventions: Arc::new(Mutex::new(Vec::new())),
        result: Payload::new(ContentType::Json, b"null".to_vec()),
        fail_result: None,
    };
    let harness = FakeHarness::new(session);
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx): (_, ControlReceiver) = mpsc::unbounded_channel();

    let mut gated = inject_command();
    gated.kind = InterventionKind::PauseResume { paused: true };
    control_tx.send(gated).unwrap();
    drop(control_tx);

    let outcome = spawn_agent(&harness, spec(), event_tx, Some(control_rx))
        .await
        .expect("a gated command does not fail the run");
    assert!(matches!(outcome, DispatchOutcome::Completed { .. }));
}

#[tokio::test]
async fn observability_only_session_runs_with_no_control_channel() {
    // Empty capability set + no control receiver: the observability-only shape.
    let session = FakeSession {
        capabilities: InterventionCapabilities::none(),
        events: Some(vec![message_event("only watching")]),
        interventions: Arc::new(Mutex::new(Vec::new())),
        result: Payload::new(ContentType::Json, b"\"ok\"".to_vec()),
        fail_result: None,
    };
    let harness = FakeHarness::new(session);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    let outcome = spawn_agent(&harness, spec(), event_tx, None)
        .await
        .expect("observability-only run completes");
    assert!(event_rx.recv().await.is_some(), "event still streamed");
    assert!(matches!(outcome, DispatchOutcome::Completed { .. }));
}

#[tokio::test]
async fn a_closed_event_sink_still_reaches_the_result() {
    let session = FakeSession {
        capabilities: InterventionCapabilities::none(),
        events: Some(vec![message_event("dropped")]),
        interventions: Arc::new(Mutex::new(Vec::new())),
        result: Payload::new(ContentType::Json, b"\"ok\"".to_vec()),
        fail_result: None,
    };
    let harness = FakeHarness::new(session);
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    // Drop the receiver up front: the sink is closed before the first send.
    drop(event_rx);

    let outcome = spawn_agent(&harness, spec(), event_tx, None)
        .await
        .expect("a closed sink does not fail the run");
    assert!(matches!(outcome, DispatchOutcome::Completed { .. }));
}

#[tokio::test]
async fn a_harness_reported_failure_surfaces_and_maps_to_failed() {
    let session = FakeSession {
        capabilities: InterventionCapabilities::none(),
        events: Some(Vec::new()),
        interventions: Arc::new(Mutex::new(Vec::new())),
        result: Payload::new(ContentType::Json, b"null".to_vec()),
        fail_result: Some(HarnessError::harness("exit code 1")),
    };
    let harness = FakeHarness::new(session);
    let (event_tx, _event_rx) = mpsc::unbounded_channel();

    let error = spawn_agent(&harness, spec(), event_tx, None)
        .await
        .expect_err("a harness-reported failure surfaces");
    assert!(matches!(error, HarnessError::Harness { .. }));

    // The caller maps it to a retryable Failed outcome.
    match harness_error_to_outcome(&error) {
        DispatchOutcome::Failed { failure } => assert!(failure.is_retryable()),
        DispatchOutcome::Completed { .. } => panic!("expected a Failed outcome"),
    }
}
