//! `ActivityContext` heartbeat, cancellation, attempt, and identifier support.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aion_core::{ActivityEvent, ActivityId, Payload, WorkflowId};
use tokio::sync::{Notify, mpsc};

use crate::error::WorkerError;

/// Handler-facing context for one activity execution.
#[derive(Clone, Debug)]
pub struct ActivityContext {
    workflow_id: Option<WorkflowId>,
    activity_id: ActivityId,
    attempt: u32,
    cancellation: Arc<CancellationState>,
    heartbeat_sender: Option<mpsc::UnboundedSender<HeartbeatRequest>>,
    /// NOI-5b agent-observability event seam (additive, OPTIONAL). A running
    /// activity (or the harness adapter driving it) emits neutral
    /// [`ActivityEvent`]s here; the worker runtime drains them and forwards them
    /// to the server's transcript sequencer over the same transport activity
    /// results take. A context created WITHOUT this seam — every isolated unit
    /// test and every activity that emits nothing — is a no-op, byte-identical to
    /// today, exactly as the `heartbeat_sender` seam is.
    event_sender: Option<mpsc::UnboundedSender<ActivityEvent>>,
}

/// Internal handle used by the worker runtime to signal cooperative cancellation.
#[derive(Clone, Debug)]
pub struct ActivityCancellationHandle {
    cancellation: Arc<CancellationState>,
}

/// Heartbeat request emitted by [`ActivityContext::heartbeat`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeartbeatRequest {
    /// Workflow owning the activity whose progress is being reported.
    pub workflow_id: WorkflowId,
    /// Activity whose progress is being reported.
    pub activity_id: ActivityId,
    /// Opaque progress detail supplied by the handler.
    pub detail: Option<Payload>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl ActivityContext {
    /// Creates a context and the internal handle that can signal cancellation.
    #[must_use]
    pub fn new(activity_id: ActivityId, attempt: u32) -> (Self, ActivityCancellationHandle) {
        Self::with_heartbeat_sender(activity_id, attempt, None)
    }

    /// Returns this activity's identifier.
    #[must_use]
    pub const fn activity_id(&self) -> &ActivityId {
        &self.activity_id
    }

    /// Returns this activity's attempt number.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Emits a cooperative heartbeat request for this activity.
    ///
    /// Only explicit handler calls enqueue heartbeats. Contexts created without a
    /// live heartbeat sender remain no-op contexts for isolated unit tests.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] when an installed heartbeat seam has been closed
    /// or when the context lacks the workflow id required by the live session.
    pub fn heartbeat(&self, detail: Option<Payload>) -> Result<(), WorkerError> {
        if let Some(sender) = &self.heartbeat_sender {
            let workflow_id = self.workflow_id.clone().ok_or_else(|| {
                WorkerError::registration(HeartbeatMissingWorkflow {
                    activity_id: self.activity_id.clone(),
                })
            })?;
            sender
                .send(HeartbeatRequest {
                    workflow_id,
                    activity_id: self.activity_id.clone(),
                    detail,
                })
                .map_err(|source| WorkerError::registration(HeartbeatSeamClosed { source }))?;
        }
        Ok(())
    }

    /// Emit a neutral agent-observability [`ActivityEvent`] onto the transcript
    /// seam (NOI-5b).
    ///
    /// Additive and OPTIONAL: on a context created without a live event seam
    /// (every isolated unit test, and every activity that does not run an
    /// instrumented agent) this is a no-op returning `Ok(())`, so behaviour is
    /// byte-identical to today. When a seam is installed the worker runtime drains
    /// these events and forwards them to the server's transcript sequencer, which
    /// stamps the commit-allocated `store_seq` — the producer never assigns it.
    ///
    /// Harness-neutral: the payload is a pure `aion-core` [`ActivityEvent`]; the
    /// per-harness mapping lives in the worker-side adapter, never here.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] when an installed event seam has been closed (the
    /// runtime drain end was dropped) — a dropped transcript event is surfaced,
    /// never silently swallowed.
    pub fn emit_event(&self, event: ActivityEvent) -> Result<(), WorkerError> {
        if let Some(sender) = &self.event_sender {
            sender
                .send(event)
                .map_err(|source| WorkerError::registration(EventSeamClosed { source }))?;
        }
        Ok(())
    }

    /// Returns true once cooperative cancellation has been signalled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.cancelled.load(Ordering::Acquire)
    }

    /// Resolves when cooperative cancellation is signalled.
    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            self.cancellation.notify.notified().await;
        }
    }

    pub(crate) fn with_heartbeat_sender(
        activity_id: ActivityId,
        attempt: u32,
        heartbeat_sender: Option<mpsc::UnboundedSender<HeartbeatRequest>>,
    ) -> (Self, ActivityCancellationHandle) {
        Self::for_workflow(None, activity_id, attempt, heartbeat_sender)
    }

    pub(crate) fn for_workflow(
        workflow_id: Option<WorkflowId>,
        activity_id: ActivityId,
        attempt: u32,
        heartbeat_sender: Option<mpsc::UnboundedSender<HeartbeatRequest>>,
    ) -> (Self, ActivityCancellationHandle) {
        Self::for_workflow_with_events(workflow_id, activity_id, attempt, heartbeat_sender, None)
    }

    /// Build a context with BOTH the heartbeat seam and the NOI-5b transcript
    /// event seam installed. The runtime uses this when an activity is driven with
    /// observability enabled; the pre-existing constructors default the event seam
    /// to `None` so every current call site is unchanged.
    pub(crate) fn for_workflow_with_events(
        workflow_id: Option<WorkflowId>,
        activity_id: ActivityId,
        attempt: u32,
        heartbeat_sender: Option<mpsc::UnboundedSender<HeartbeatRequest>>,
        event_sender: Option<mpsc::UnboundedSender<ActivityEvent>>,
    ) -> (Self, ActivityCancellationHandle) {
        let cancellation = Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        });
        let context = Self {
            workflow_id,
            activity_id,
            attempt,
            cancellation: Arc::clone(&cancellation),
            heartbeat_sender,
            event_sender,
        };
        let handle = ActivityCancellationHandle { cancellation };
        (context, handle)
    }
}

impl ActivityCancellationHandle {
    /// Signals cooperative cancellation to the handler-facing context.
    pub fn cancel(&self) {
        let was_cancelled = self.cancellation.cancelled.swap(true, Ordering::AcqRel);
        if !was_cancelled {
            self.cancellation.notify.notify_waiters();
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("activity heartbeat seam is closed: {source}")]
struct HeartbeatSeamClosed {
    source: mpsc::error::SendError<HeartbeatRequest>,
}

#[derive(Debug, thiserror::Error)]
#[error("activity transcript event seam is closed: {source}")]
struct EventSeamClosed {
    source: mpsc::error::SendError<ActivityEvent>,
}

#[derive(Debug, thiserror::Error)]
#[error("activity {activity_id} heartbeat is missing workflow id")]
struct HeartbeatMissingWorkflow {
    activity_id: ActivityId,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aion_core::ActivityId;

    use super::ActivityContext;

    /// NOI-5b: a context WITHOUT an event seam is a no-op — `emit_event` returns
    /// `Ok(())` and drops the event, byte-identical to a context that predates the
    /// seam. This is the additive guarantee: an activity that emits nothing (and
    /// every isolated unit test) is unaffected.
    #[tokio::test]
    async fn emit_event_is_a_no_op_without_an_installed_seam() {
        use aion_core::{ActivityEvent, ActivityEventKind, MessageRole, WorkflowId};
        use chrono::Utc;
        use uuid::Uuid;

        let (context, _cancellation) =
            ActivityContext::new(ActivityId::from_sequence_position(1), 0);
        let event = ActivityEvent {
            workflow_id: WorkflowId::new(Uuid::from_u128(1)),
            activity_id: ActivityId::from_sequence_position(1),
            attempt: 0,
            agent_id: Uuid::from_u128(2),
            agent_role: "orchestrator".to_owned(),
            emitted_at: Utc::now(),
            worker_seq: 1,
            store_seq: None,
            ephemeral: false,
            kind: ActivityEventKind::Message {
                role: MessageRole::Assistant,
                text: "hello".to_owned(),
            },
        };
        assert!(context.emit_event(event).is_ok());
    }

    /// With an event seam installed, `emit_event` forwards the neutral event to
    /// the runtime drain end — the additive worker->server ingestion seam.
    #[tokio::test]
    async fn emit_event_forwards_to_installed_seam() -> Result<(), Box<dyn std::error::Error>> {
        use aion_core::{ActivityEvent, ActivityEventKind, MessageRole, WorkflowId};
        use chrono::Utc;
        use uuid::Uuid;

        let (sender, mut drain) = super::mpsc::unbounded_channel();
        let (context, _cancellation) = ActivityContext::for_workflow_with_events(
            Some(WorkflowId::new(Uuid::from_u128(1))),
            ActivityId::from_sequence_position(1),
            0,
            None,
            Some(sender),
        );
        let event = ActivityEvent {
            workflow_id: WorkflowId::new(Uuid::from_u128(1)),
            activity_id: ActivityId::from_sequence_position(1),
            attempt: 0,
            agent_id: Uuid::from_u128(2),
            agent_role: "orchestrator".to_owned(),
            emitted_at: Utc::now(),
            worker_seq: 7,
            store_seq: None,
            ephemeral: false,
            kind: ActivityEventKind::Message {
                role: MessageRole::Assistant,
                text: "steer".to_owned(),
            },
        };
        context.emit_event(event.clone())?;
        let delivered = drain.recv().await.ok_or("event must be delivered")?;
        assert_eq!(delivered.worker_seq, 7);
        assert_eq!(delivered, event);
        Ok(())
    }

    #[tokio::test]
    async fn context_exposes_identity_attempt_and_cancellation_signal() {
        let activity_id = ActivityId::from_sequence_position(42);
        let (context, cancellation) = ActivityContext::new(activity_id.clone(), 3);

        assert_eq!(context.activity_id(), &activity_id);
        assert_eq!(context.attempt(), 3);
        assert!(!context.is_cancelled());

        cancellation.cancel();

        assert!(context.is_cancelled());
        let cancelled = tokio::time::timeout(Duration::from_millis(50), context.cancelled()).await;
        assert!(cancelled.is_ok());
    }
}
