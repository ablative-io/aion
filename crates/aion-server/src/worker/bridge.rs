//! NIF bridge dispatcher that routes `run_activity` calls to connected workers.
//!
//! `WorkerActivityDispatcher` implements `aion::ActivityDispatcher` so the
//! engine's `aion_flow_ffi:run_activity` NIF can synchronously dispatch to a
//! remote worker and block until the result comes back.
//!
//! The entire dispatch path is sync — no `Handle::block_on` or tokio context
//! required. Task send uses `try_send()` (non-blocking channel push), and the
//! response wait uses `std::sync::mpsc` (blocks the beamr dirty scheduler
//! thread without touching the tokio runtime).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aion::ActivityDispatcher;
use aion_core::{ActivityId, ContentType, Payload, WorkflowId};
use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
use dashmap::DashMap;

use super::dispatch::{ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink};
use super::heartbeat::{HeartbeatTracker, InFlightActivity};
use super::registry::{ConnectedWorkerRegistry, WorkerMessage};
use crate::error::ServerError;
use crate::shutdown::DrainState;

type SyncSender = std::sync::mpsc::SyncSender<Result<String, String>>;
type SyncReceiver = std::sync::mpsc::Receiver<Result<String, String>>;

/// Tracks in-flight activity dispatches waiting for worker results.
///
/// When the server's worker stream handler receives an `ActivityResult`, it
/// calls [`complete_activity`](ActivityCompletionSink::complete_activity) to
/// deliver the result to the blocked NIF thread.
#[derive(Clone, Debug, Default)]
pub struct PendingActivities {
    pending: Arc<DashMap<ActivityId, SyncSender>>,
}

impl PendingActivities {
    fn insert(&self, activity_id: ActivityId) -> SyncReceiver {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.pending.insert(activity_id, tx);
        rx
    }

    fn complete(&self, activity_id: &ActivityId, result: Result<String, String>) -> bool {
        if let Some((_, sender)) = self.pending.remove(activity_id) {
            sender.send(result).is_ok()
        } else {
            false
        }
    }
}

impl ActivityCompletionSink for PendingActivities {
    fn complete_activity(&self, completion: ActivityCompletion) -> Result<(), ServerError> {
        let result = match completion.outcome {
            ActivityCompletionOutcome::Succeeded(payload) => {
                payload_to_string(&payload).map_err(|e| {
                    ServerError::worker_dispatch("", "", format!("payload decode: {e}"))
                })?
            }
            ActivityCompletionOutcome::Failed(error) => {
                let prefix = if error.is_retryable() {
                    "retryable"
                } else {
                    "terminal"
                };
                Err(format!("{prefix}:{}", error.message))
            }
        };
        self.complete(&completion.activity_id, result);
        Ok(())
    }
}

fn payload_to_string(payload: &Payload) -> Result<Result<String, String>, String> {
    match payload.content_type() {
        ContentType::Json => String::from_utf8(payload.bytes().to_vec())
            .map(Ok)
            .map_err(|_| "activity result payload is not valid UTF-8".to_owned()),
    }
}

/// Dispatcher that routes `run_activity` NIF calls to connected workers.
///
/// Fully synchronous — uses `try_send` for the task channel and
/// `std::sync::mpsc::Receiver::recv_timeout` for the response, so no
/// tokio runtime context is needed on the calling thread.
pub struct WorkerActivityDispatcher {
    registry: ConnectedWorkerRegistry,
    namespace: String,
    pending: PendingActivities,
    heartbeat_tracker: HeartbeatTracker,
    drain_state: DrainState,
    next_id: AtomicU64,
    timeout: Duration,
}

impl std::fmt::Debug for WorkerActivityDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerActivityDispatcher")
            .field("namespace", &self.namespace)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl WorkerActivityDispatcher {
    /// Build a dispatcher for the given namespace and worker registry.
    #[must_use]
    pub fn new(registry: ConnectedWorkerRegistry, namespace: impl Into<String>) -> Self {
        Self {
            registry,
            namespace: namespace.into(),
            pending: PendingActivities::default(),
            heartbeat_tracker: HeartbeatTracker::new(Duration::from_secs(30)),
            drain_state: DrainState::default(),
            next_id: AtomicU64::new(1),
            timeout: Duration::from_secs(30),
        }
    }

    /// Share a caller-supplied pending-activities tracker.
    #[must_use]
    pub fn with_pending(mut self, pending: PendingActivities) -> Self {
        self.pending = pending;
        self
    }

    /// Share a caller-supplied heartbeat/liveness tracker.
    #[must_use]
    pub fn with_heartbeat_tracker(mut self, heartbeat_tracker: HeartbeatTracker) -> Self {
        self.heartbeat_tracker = heartbeat_tracker;
        self
    }

    /// Share the server drain gate.
    #[must_use]
    pub fn with_drain_state(mut self, drain_state: DrainState) -> Self {
        self.drain_state = drain_state;
        self
    }

    /// Override the per-activity dispatch timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl ActivityDispatcher for WorkerActivityDispatcher {
    fn dispatch(&self, name: &str, input: &str, config: &str) -> Result<String, String> {
        let _ = config;
        self.drain_state
            .ensure_accepting(&self.namespace, name)
            .map_err(|error| error.to_string())?;

        let seq = self.next_id.fetch_add(1, Ordering::Relaxed);
        let activity_id = ActivityId::from_sequence_position(seq);
        let workflow_id = WorkflowId::new_v4();

        let worker = self
            .registry
            .select_worker(&self.namespace, name)
            .map_err(|e| format!("registry error: {e}"))?
            .ok_or_else(|| {
                format!(
                    "no connected worker for activity type '{name}' in namespace '{}'",
                    self.namespace
                )
            })?;
        self.drain_state
            .ensure_accepting(&self.namespace, name)
            .map_err(|error| error.to_string())?;

        let task = ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
            activity_id: Some(ProtoActivityId::from(activity_id.clone())),
            activity_type: name.to_owned(),
            input: Some(ProtoPayload {
                content_type: String::from("application/json"),
                bytes: input.as_bytes().to_vec(),
            }),
        };

        let rx = self.pending.insert(activity_id.clone());
        self.heartbeat_tracker
            .track_task(
                worker.id(),
                InFlightActivity {
                    workflow_id: workflow_id.clone(),
                    activity_id: activity_id.clone(),
                },
                std::time::Instant::now(),
            )
            .map_err(|error| error.to_string())?;

        if let Err(error) = worker.sender().try_send(WorkerMessage::ActivityTask(task)) {
            let _removed = self.pending.pending.remove(&activity_id);
            let _ = self
                .heartbeat_tracker
                .complete_task(worker.id(), &workflow_id, &activity_id);
            self.drain_state.notify_activity_drained();
            return Err(format!("worker task channel full or closed: {error}"));
        }

        match rx.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.pending.pending.remove(&activity_id);
                let _ =
                    self.heartbeat_tracker
                        .complete_task(worker.id(), &workflow_id, &activity_id);
                self.drain_state.notify_activity_drained();
                Err(format!(
                    "activity '{name}' timed out after {}s",
                    self.timeout.as_secs()
                ))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("activity response channel dropped".to_owned())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityError, ActivityErrorKind, ContentType, Payload};

    use super::*;

    fn activity_id(pos: u64) -> ActivityId {
        ActivityId::from_sequence_position(pos)
    }

    #[test]
    fn pending_insert_and_complete_delivers_result() {
        let pending = PendingActivities::default();
        let id = activity_id(1);
        let rx = pending.insert(id.clone());

        assert!(pending.complete(&id, Ok("done".to_owned())));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)),
            Ok(Ok("done".to_owned()))
        );
    }

    #[test]
    fn pending_complete_unknown_returns_false() {
        let pending = PendingActivities::default();
        assert!(!pending.complete(&activity_id(99), Ok("orphan".to_owned())));
    }

    #[test]
    fn completion_sink_routes_success() -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        let id = activity_id(2);
        let rx = pending.insert(id.clone());
        let payload = Payload::new(ContentType::Json, br#"{"greeting":"hi"}"#.to_vec());

        pending.complete_activity(ActivityCompletion {
            workflow_id: WorkflowId::new_v4(),
            activity_id: id,
            outcome: ActivityCompletionOutcome::Succeeded(payload),
        })?;

        let result = rx
            .recv_timeout(Duration::from_millis(50))
            .map_err(|e| ServerError::worker_dispatch("", "", format!("channel: {e}")))?;
        assert_eq!(result, Ok(r#"{"greeting":"hi"}"#.to_owned()));
        Ok(())
    }

    #[test]
    fn completion_sink_routes_retryable_error() -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        let id = activity_id(3);
        let rx = pending.insert(id.clone());

        pending.complete_activity(ActivityCompletion {
            workflow_id: WorkflowId::new_v4(),
            activity_id: id,
            outcome: ActivityCompletionOutcome::Failed(ActivityError {
                kind: ActivityErrorKind::Retryable,
                message: "temporary".to_owned(),
                details: None,
            }),
        })?;

        let result = rx
            .recv_timeout(Duration::from_millis(50))
            .map_err(|e| ServerError::worker_dispatch("", "", format!("channel: {e}")))?;
        assert_eq!(result, Err("retryable:temporary".to_owned()));
        Ok(())
    }

    #[test]
    fn dispatcher_returns_error_when_no_worker_registered() {
        let registry = ConnectedWorkerRegistry::default();
        let dispatcher = WorkerActivityDispatcher::new(registry, "default");

        let result = dispatcher.dispatch("greet", "{}", "{}");

        assert!(result.is_err());
        let err = result.err().unwrap_or_default();
        assert!(
            err.contains("no connected worker"),
            "unexpected error: {err}"
        );
    }
}
