//! [`MockAgentHarness`] — the second, INTERVENEABLE shape driven in NOI-8.
//!
//! Where [`crate::CliHarness`] proves the **observability-only** tier (empty capability set), this
//! deterministic in-crate harness proves the **interveneable** tier: it advertises
//! `{inject_message, cancel}`, accepts those commands (recording them so a test can assert the
//! neutral command reached the agent), and REJECTS at least one advertised-unsupported primitive
//! (`RespondToApproval`, and the other two it does not advertise) with a clean
//! [`HarnessError::CapabilityNotSupported`] NACK.
//!
//! Together the two harnesses exercise BOTH branches of the [`AgentSession::intervene`] contract —
//! the empty-set rejection and the advertised-set accept/gate — through the same neutral seam,
//! proving `aion-integrations` is a real SDK with two independent implementations, not a Norn
//! wrapper (§9.1, NOI-8).
//!
//! It is a genuine `AgentHarness` implementation (not a stub returning canned outcomes): the worker
//! driver ([`aion_worker::spawn_agent`]) drives it live in the crate's e2e test, feeding real
//! commands through the real control channel and observing the real accept/gate results.

use std::sync::Arc;
use std::sync::Mutex;

use aion_core::{
    ActivityEvent, ActivityEventKind, ContentType, InterventionCapabilities, InterventionCommand,
    InterventionKind, InterventionPrimitive, MessageRole, Payload, StopKind,
};
use aion_integrations::contract::{AgentHarness, AgentSession};
use aion_integrations::error::HarnessError;
use aion_integrations::spec::AgentRunSpec;
use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use uuid::Uuid;

/// A record of the commands a [`MockAgentSession`] accepted, shared with the test that drives it.
pub type AppliedLog = Arc<Mutex<Vec<InterventionKind>>>;

/// A handle that keeps a [`MockAgentSession`]'s run alive and ends it on demand.
///
/// The session's event stream drains a channel; holding this sender keeps the stream open (the run
/// stays live so a driver can deliver commands mid-run), and dropping it — via [`Self::end_run`] or
/// simply letting it fall out of scope — ends the stream, at which point the worker driver takes the
/// terminal result. This mirrors the worker's real lifecycle, where the run ends when the harness's
/// event stream closes, not when a command arrives.
pub struct RunHandle {
    keep_alive: mpsc::UnboundedSender<ActivityEvent>,
}

impl RunHandle {
    /// Ends the run by closing the event stream, so the driving worker takes the terminal result.
    pub fn end_run(self) {
        drop(self.keep_alive);
    }
}

/// An interveneable mock harness advertising `{inject_message, cancel}`.
///
/// Deterministic: it replays a fixed two-event transcript (a message + a terminal stop), keeps the
/// run live until the paired [`RunHandle`] is dropped, and yields a fixed result. The `applied` log
/// is shared so a driver can assert an accepted command reached the session; commands outside the
/// advertised set are gated and never logged.
#[derive(Clone)]
pub struct MockAgentHarness {
    applied: AppliedLog,
    run_handle: Arc<Mutex<Option<RunHandle>>>,
}

impl Default for MockAgentHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAgentHarness {
    /// A mock harness with a fresh applied-command log.
    #[must_use]
    pub fn new() -> Self {
        Self {
            applied: Arc::new(Mutex::new(Vec::new())),
            run_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// The shared log of commands sessions from this harness have accepted.
    #[must_use]
    pub fn applied(&self) -> AppliedLog {
        Arc::clone(&self.applied)
    }

    /// Takes the [`RunHandle`] for the most recently started run, if a run has started and its
    /// handle has not already been taken.
    ///
    /// A driver ends the run by dropping this handle (or calling [`RunHandle::end_run`]); until then
    /// the run stays live so commands can be delivered mid-run.
    #[must_use]
    pub fn take_run_handle(&self) -> Option<RunHandle> {
        self.run_handle
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
    }

    /// The capability set this harness advertises: `{inject_message, cancel}` — a strict subset of
    /// the five neutral primitives, so `pause_resume`, `update_budget`, and `respond_to_approval`
    /// are all advertised-unsupported and gated.
    #[must_use]
    pub fn advertised_capabilities() -> InterventionCapabilities {
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ])
    }
}

/// The live session the mock produces: it streams a fixed transcript, gates commands on the
/// advertised set, and drains an event channel whose sender (the [`RunHandle`]) a driver holds so
/// the run stays live until the driver ends it.
pub struct MockAgentSession {
    capabilities: InterventionCapabilities,
    applied: AppliedLog,
    /// A channel draining into the events stream. The paired sender is held by the [`RunHandle`];
    /// dropping it ends the stream (and the run). A fixed transcript is pre-loaded before the
    /// handle is published.
    events: Option<mpsc::UnboundedReceiver<ActivityEvent>>,
    result: Payload,
}

#[async_trait]
impl AgentHarness for MockAgentHarness {
    type Session = MockAgentSession;

    async fn start(&self, spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        let (tx, rx) = mpsc::unbounded_channel();
        // Pre-load a fixed transcript stamped with the spec identity. The run stays live until the
        // RunHandle (holding `tx`) is dropped by the driver, at which point the stream ends.
        for event in fixed_transcript(&spec) {
            let _ = tx.send(event);
        }
        if let Ok(mut guard) = self.run_handle.lock() {
            *guard = Some(RunHandle { keep_alive: tx });
        }
        Ok(MockAgentSession {
            capabilities: MockAgentHarness::advertised_capabilities(),
            applied: Arc::clone(&self.applied),
            events: Some(rx),
            result: Payload::new(ContentType::Json, b"{\"result\":\"completed\"}".to_vec()),
        })
    }
}

/// A fixed two-event transcript stamped with the run identity: one assistant message + a stop.
fn fixed_transcript(spec: &AgentRunSpec) -> Vec<ActivityEvent> {
    let base = ActivityEvent {
        workflow_id: spec.workflow_id.clone(),
        activity_id: spec.activity_id.clone(),
        attempt: spec.attempt,
        agent_id: Uuid::nil(),
        agent_role: "mock".to_owned(),
        emitted_at: Utc::now(),
        worker_seq: 0,
        store_seq: None,
        ephemeral: false,
        kind: ActivityEventKind::Message {
            role: MessageRole::Assistant,
            text: "working".to_owned(),
        },
    };
    let stop = ActivityEvent {
        worker_seq: 1,
        kind: ActivityEventKind::Stop {
            reason: StopKind::EndTurn,
        },
        ..base.clone()
    };
    vec![base, stop]
}

#[async_trait]
impl AgentSession for MockAgentSession {
    fn capabilities(&self) -> &InterventionCapabilities {
        &self.capabilities
    }

    fn events(&mut self) -> BoxStream<'static, ActivityEvent> {
        match self.events.take() {
            Some(receiver) => Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(
                receiver,
            )),
            None => Box::pin(futures::stream::empty()),
        }
    }

    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError> {
        // Capability-gate on the advertised set: inject_message/cancel are accepted; the three
        // unadvertised primitives (pause_resume/update_budget/respond_to_approval) are cleanly
        // rejected as capability-not-supported and NEVER recorded as applied.
        if !self.capabilities.supports(&cmd.kind) {
            return Err(HarnessError::capability_not_supported(format!(
                "{:?}",
                cmd.kind.primitive()
            )));
        }
        self.applied
            .lock()
            .map_err(|_poison| HarnessError::harness("applied-command lock poisoned"))?
            .push(cmd.kind);
        Ok(())
    }

    async fn wait_result(self) -> Result<Payload, HarnessError> {
        // The run has already ended (the driver dropped the RunHandle, closing the event stream);
        // the terminal result is the fixed completed payload.
        Ok(self.result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use aion_core::{ActivityId, ApprovalDecision, InjectPriority, WorkflowId};
    use futures::StreamExt;

    fn spec() -> AgentRunSpec {
        AgentRunSpec::new(
            WorkflowId::new(Uuid::nil()),
            ActivityId::from_sequence_position(4),
            2,
            Payload::new(ContentType::Json, b"\"in\"".to_vec()),
        )
    }

    fn command(kind: InterventionKind) -> InterventionCommand {
        InterventionCommand {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(4),
            attempt: 2,
            issued_by: Some("operator".to_owned()),
            issued_at: Utc::now(),
            kind,
        }
    }

    #[tokio::test]
    async fn advertises_inject_message_and_cancel_only() {
        let harness = MockAgentHarness::new();
        let session = harness.start(spec()).await.unwrap();
        let caps = session.capabilities();
        assert!(caps.supports_primitive(InterventionPrimitive::InjectMessage));
        assert!(caps.supports_primitive(InterventionPrimitive::Cancel));
        assert!(!caps.supports_primitive(InterventionPrimitive::PauseResume));
        assert!(!caps.supports_primitive(InterventionPrimitive::UpdateBudget));
        assert!(!caps.supports_primitive(InterventionPrimitive::RespondToApproval));
    }

    #[tokio::test]
    async fn accepts_an_advertised_command_and_records_it() {
        let harness = MockAgentHarness::new();
        let applied = harness.applied();
        let session = harness.start(spec()).await.unwrap();

        session
            .intervene(command(InterventionKind::InjectMessage {
                text: "steer".to_owned(),
                priority: InjectPriority::Interrupt,
            }))
            .await
            .unwrap();
        session
            .intervene(command(InterventionKind::Cancel {
                reason: "stop".to_owned(),
            }))
            .await
            .unwrap();

        assert_eq!(applied.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rejects_respond_to_approval_as_capability_not_supported() {
        let harness = MockAgentHarness::new();
        let applied = harness.applied();
        let session = harness.start(spec()).await.unwrap();

        let error = session
            .intervene(command(InterventionKind::RespondToApproval {
                call_id: "c1".to_owned(),
                decision: ApprovalDecision::Approve,
                note: None,
            }))
            .await
            .unwrap_err();
        assert!(
            matches!(error, HarnessError::CapabilityNotSupported { .. }),
            "an advertised-unsupported primitive is cleanly NACKed, got {error:?}"
        );
        assert!(
            applied.lock().unwrap().is_empty(),
            "a gated command is never recorded as applied"
        );
    }

    #[tokio::test]
    async fn streams_a_transcript_and_returns_a_result() {
        let harness = MockAgentHarness::new();
        let mut session = harness.start(spec()).await.unwrap();
        // End the run so the pre-loaded transcript stream terminates and can be collected.
        harness.take_run_handle().expect("a run handle").end_run();
        let events: Vec<ActivityEvent> = session.events().collect().await;
        assert_eq!(events.len(), 2, "the fixed transcript streams");
        assert!(matches!(events[0].kind, ActivityEventKind::Message { .. }));
        assert!(matches!(events[1].kind, ActivityEventKind::Stop { .. }));
        let payload = session.wait_result().await.unwrap();
        assert_eq!(payload.content_type(), &ContentType::Json);
    }
}
