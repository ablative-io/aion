//! NIF bridge dispatcher that routes `run_activity` calls to connected workers.
//!
//! `WorkerActivityDispatcher` implements `aion::ActivityDispatcher` so the
//! engine's `aion_flow_ffi:run_activity` NIF can synchronously dispatch to a
//! remote worker and block until the result comes back.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aion::ActivityDispatcher;
use aion_core::{ActivityId, ContentType, Payload, WorkflowId};
use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
use dashmap::DashMap;
use tokio::sync::oneshot;

use super::dispatch::{ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink};
use super::registry::ConnectedWorkerRegistry;
use crate::error::ServerError;

/// Tracks in-flight activity dispatches waiting for worker results.
///
/// When the server's worker stream handler receives an `ActivityResult`, it
/// calls [`complete_activity`](ActivityCompletionSink::complete_activity) to
/// deliver the result to the blocked NIF thread.
#[derive(Clone, Debug, Default)]
pub struct PendingActivities {
    pending: Arc<DashMap<ActivityId, oneshot::Sender<Result<String, String>>>>,
}

impl PendingActivities {
    fn insert(&self, activity_id: ActivityId) -> oneshot::Receiver<Result<String, String>> {
        let (tx, rx) = oneshot::channel();
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
/// The dispatcher holds a tokio runtime handle so it can bridge the
/// synchronous NIF call (from beamr's dirty scheduler thread) to the
/// async worker channel.
pub struct WorkerActivityDispatcher {
    registry: ConnectedWorkerRegistry,
    namespace: String,
    pending: PendingActivities,
    next_id: AtomicU64,
    runtime: tokio::runtime::Handle,
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
    pub fn new(
        registry: ConnectedWorkerRegistry,
        namespace: impl Into<String>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            registry,
            namespace: namespace.into(),
            pending: PendingActivities::default(),
            next_id: AtomicU64::new(1),
            runtime,
            timeout: Duration::from_secs(30),
        }
    }

    /// Share a caller-supplied pending-activities tracker.
    #[must_use]
    pub fn with_pending(mut self, pending: PendingActivities) -> Self {
        self.pending = pending;
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

        let task = ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id.clone())),
            activity_type: name.to_owned(),
            input: Some(ProtoPayload {
                content_type: String::from("application/json"),
                bytes: input.as_bytes().to_vec(),
            }),
        };

        let rx = self.pending.insert(activity_id.clone());
        let sender = worker.sender().clone();
        let timeout = self.timeout;
        let pending = self.pending.clone();

        self.runtime.block_on(async {
            sender
                .send(task)
                .await
                .map_err(|_| "worker stream closed before task could be sent".to_owned())?;

            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("activity response channel dropped".to_owned()),
                Err(_) => {
                    pending.pending.remove(&activity_id);
                    Err(format!(
                        "activity '{name}' timed out after {}s",
                        timeout.as_secs()
                    ))
                }
            }
        })
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
        let mut rx = pending.insert(id.clone());

        assert!(pending.complete(&id, Ok("done".to_owned())));
        assert_eq!(rx.try_recv(), Ok(Ok("done".to_owned())));
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
        let mut rx = pending.insert(id.clone());
        let payload = Payload::new(ContentType::Json, br#"{"greeting":"hi"}"#.to_vec());

        pending.complete_activity(ActivityCompletion {
            workflow_id: WorkflowId::new_v4(),
            activity_id: id,
            outcome: ActivityCompletionOutcome::Succeeded(payload),
        })?;

        let result = rx
            .try_recv()
            .map_err(|e| ServerError::worker_dispatch("", "", format!("channel: {e}")))?;
        assert_eq!(result, Ok(r#"{"greeting":"hi"}"#.to_owned()));
        Ok(())
    }

    #[test]
    fn completion_sink_routes_retryable_error() -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        let id = activity_id(3);
        let mut rx = pending.insert(id.clone());

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
            .try_recv()
            .map_err(|e| ServerError::worker_dispatch("", "", format!("channel: {e}")))?;
        assert_eq!(result, Err("retryable:temporary".to_owned()));
        Ok(())
    }

    #[test]
    fn dispatcher_returns_error_when_no_worker_registered() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|_| std::process::abort());
        let registry = ConnectedWorkerRegistry::default();
        let dispatcher = WorkerActivityDispatcher::new(registry, "default", rt.handle().clone());

        let result = dispatcher.dispatch("greet", "{}", "{}");

        assert!(result.is_err());
        let err = result.err().unwrap_or_default();
        assert!(
            err.contains("no connected worker"),
            "unexpected error: {err}"
        );
    }
}
