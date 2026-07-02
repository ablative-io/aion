//! An in-crate test double that implements the [`AgentHarness`] / [`AgentSession`] seam, proving:
//!
//! 1. the trait is **object-usable** (a session drives events + intervene + result through the
//!    trait, and a harness is usable behind `Box<dyn AgentHarness>`), and
//! 2. the **observability-only** case (empty capability set) is first-class: `intervene()`
//!    returns `CapabilityNotSupported`, `events()` still yields `ActivityEvent`s, and
//!    `wait_result()` still returns a `Payload`.
//!
//! This is the NOI-3 seam proof; the real Norn adapter is NOI-4 and lives elsewhere.

use std::sync::Arc;
use std::sync::Mutex;

use aion_integrations::{
    ActivityEvent, ActivityEventKind, ActivityId, AgentHarness, AgentRunSpec, AgentSession,
    ContentType, HarnessError, InterventionCapabilities, InterventionCommand, InterventionKind,
    InterventionPrimitive, MessageRole, Payload, StopKind, WorkflowId,
};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use uuid::Uuid;

/// A mock harness that replays a fixed transcript and yields a fixed result. Its capability set is
/// configurable so one double covers both the rich case and the observability-only case.
struct MockHarness {
    capabilities: InterventionCapabilities,
    transcript: Vec<ActivityEvent>,
    result: Payload,
}

/// The live session the mock produces. `intervene()` records accepted commands so a test can
/// assert routing, and gates every command on the advertised set.
struct MockSession {
    capabilities: InterventionCapabilities,
    transcript: Vec<ActivityEvent>,
    result: Payload,
    accepted: Arc<Mutex<Vec<InterventionCommand>>>,
}

#[async_trait]
impl AgentHarness for MockHarness {
    type Session = MockSession;

    async fn start(&self, spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        // Stamp the spec's identity onto every replayed event, proving `start` sees the run
        // identity and nothing harness-specific.
        let AgentRunSpec {
            workflow_id,
            activity_id,
            attempt,
            ..
        } = spec;
        let transcript = self
            .transcript
            .iter()
            .cloned()
            .map(|mut event| {
                event.workflow_id = workflow_id.clone();
                event.activity_id = activity_id.clone();
                event.attempt = attempt;
                event
            })
            .collect();
        Ok(MockSession {
            capabilities: self.capabilities.clone(),
            transcript,
            result: self.result.clone(),
            accepted: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

#[async_trait]
impl AgentSession for MockSession {
    fn capabilities(&self) -> &InterventionCapabilities {
        &self.capabilities
    }

    fn events(&mut self) -> BoxStream<'static, ActivityEvent> {
        stream::iter(std::mem::take(&mut self.transcript)).boxed()
    }

    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError> {
        if !self.capabilities.supports(&cmd.kind) {
            return Err(HarnessError::capability_not_supported(format!(
                "{:?}",
                cmd.kind.primitive()
            )));
        }
        self.accepted
            .lock()
            .map_err(|_poison| HarnessError::harness("accepted-command lock poisoned"))?
            .push(cmd);
        Ok(())
    }

    async fn wait_result(self) -> Result<Payload, HarnessError> {
        Ok(self.result)
    }
}

fn message_event(text: &str) -> ActivityEvent {
    ActivityEvent {
        workflow_id: WorkflowId::new(Uuid::nil()),
        activity_id: ActivityId::from_sequence_position(0),
        attempt: 0,
        agent_id: Uuid::nil(),
        agent_role: "agent".to_owned(),
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

fn stop_event() -> ActivityEvent {
    ActivityEvent {
        kind: ActivityEventKind::Stop {
            reason: StopKind::EndTurn,
        },
        ..message_event("")
    }
}

fn run_spec() -> AgentRunSpec {
    AgentRunSpec::new(
        WorkflowId::new_v4(),
        ActivityId::from_sequence_position(9),
        3,
        "mock-activity",
        Payload::new(ContentType::Json, b"{\"task\":\"x\"}".to_vec()),
    )
}

/// Drives a harness entirely through the trait — including behind a trait object — to prove the
/// seam is object-usable.
async fn drive(
    harness: Box<dyn AgentHarness<Session = MockSession>>,
    command: Option<InterventionCommand>,
) -> Result<(Vec<ActivityEvent>, Result<(), HarnessError>, Payload), HarnessError> {
    let spec = run_spec();
    let expected_attempt = spec.attempt;
    let mut session = harness.start(spec).await?;

    let events: Vec<ActivityEvent> = session.events().collect().await;
    // Every event carries the spec identity `start` stamped.
    for event in &events {
        assert_eq!(event.attempt, expected_attempt);
    }

    let intervene_outcome = match command {
        Some(cmd) => session.intervene(cmd).await,
        None => Ok(()),
    };

    let result = session.wait_result().await?;
    Ok((events, intervene_outcome, result))
}

#[tokio::test]
async fn rich_harness_streams_events_accepts_supported_command_and_returns_result()
-> Result<(), HarnessError> {
    let capabilities = InterventionCapabilities::from_primitives([
        InterventionPrimitive::InjectMessage,
        InterventionPrimitive::Cancel,
    ]);
    let harness = MockHarness {
        capabilities,
        transcript: vec![message_event("working"), stop_event()],
        result: Payload::new(ContentType::Json, b"{\"ok\":true}".to_vec()),
    };

    let command = InterventionCommand {
        workflow_id: WorkflowId::new_v4(),
        activity_id: ActivityId::from_sequence_position(9),
        attempt: 3,
        issued_by: Some("operator".to_owned()),
        issued_at: Utc::now(),
        kind: InterventionKind::Cancel {
            reason: "stop now".to_owned(),
        },
    };

    let (events, intervene_outcome, result) = drive(Box::new(harness), Some(command)).await?;

    assert_eq!(events.len(), 2, "both transcript events stream out");
    assert!(
        intervene_outcome.is_ok(),
        "a supported command is accepted: {intervene_outcome:?}"
    );
    assert_eq!(result.bytes(), b"{\"ok\":true}");
    Ok(())
}

#[tokio::test]
async fn observability_only_harness_is_first_class() -> Result<(), HarnessError> {
    // The empty capability set: observability-only.
    let harness = MockHarness {
        capabilities: InterventionCapabilities::none(),
        transcript: vec![message_event("read-only run"), stop_event()],
        result: Payload::new(ContentType::Json, b"{\"done\":true}".to_vec()),
    };

    let command = InterventionCommand {
        workflow_id: WorkflowId::new_v4(),
        activity_id: ActivityId::from_sequence_position(9),
        attempt: 3,
        issued_by: None,
        issued_at: Utc::now(),
        kind: InterventionKind::Cancel {
            reason: "try to cancel".to_owned(),
        },
    };

    let (events, intervene_outcome, result) = drive(Box::new(harness), Some(command)).await?;

    // events() STILL yields ActivityEvents even with no intervention capability.
    assert_eq!(events.len(), 2, "observability still streams a transcript");
    // intervene() returns capability-not-supported for the empty set.
    assert!(
        matches!(
            intervene_outcome,
            Err(HarnessError::CapabilityNotSupported { .. })
        ),
        "empty caps reject every command: {intervene_outcome:?}"
    );
    // wait_result() STILL returns a Payload.
    assert_eq!(result.bytes(), b"{\"done\":true}");
    Ok(())
}

#[tokio::test]
async fn unadvertised_command_is_rejected_even_when_some_are_supported() -> Result<(), HarnessError>
{
    // Supports only InjectMessage; a Cancel must be rejected as unsupported.
    let harness = MockHarness {
        capabilities: InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
        ]),
        transcript: vec![stop_event()],
        result: Payload::new(ContentType::Json, b"{}".to_vec()),
    };

    let command = InterventionCommand {
        workflow_id: WorkflowId::new_v4(),
        activity_id: ActivityId::from_sequence_position(9),
        attempt: 3,
        issued_by: None,
        issued_at: Utc::now(),
        kind: InterventionKind::Cancel {
            reason: "unsupported here".to_owned(),
        },
    };

    let (_events, intervene_outcome, _result) = drive(Box::new(harness), Some(command)).await?;

    assert!(
        matches!(
            intervene_outcome,
            Err(HarnessError::CapabilityNotSupported { .. })
        ),
        "a primitive outside the advertised set is rejected: {intervene_outcome:?}"
    );
    Ok(())
}
