//! NIF bridge dispatcher that routes `run_activity` calls to connected workers.
//!
//! `WorkerActivityDispatcher` implements `aion::ActivityDispatcher` so the
//! engine's activity NIFs can synchronously dispatch to a remote worker and
//! block until the result comes back.
//!
//! # Threading contract
//!
//! The engine invokes [`aion::ActivityDispatcher::dispatch`] from two kinds of
//! threads: beamr scheduler threads (concurrency combinators) and spawned
//! tokio tasks (the two-phase `dispatch_activity` completion task). The task
//! send uses `try_send()` (non-blocking channel push) and the response wait
//! blocks on `std::sync::mpsc::Receiver::recv_timeout`.
//!
//! Blocking is harmless on a beamr thread, but on a tokio runtime worker it
//! must be wrapped in `tokio::task::block_in_place`: the `try_send` wakes the
//! per-worker gRPC stream forwarder task, and tokio schedules a task woken
//! from task context into the *current* worker's LIFO slot, which no other
//! runtime worker can steal. Without the `block_in_place` core handoff the
//! forwarder sits trapped in that slot while this thread blocks, so the queued
//! `ActivityTask` is only flushed to the worker when the timeout fires — every
//! remote activity fails with `ActivityTimeout` even though the worker is
//! healthy. `block_in_place` moves the worker's scheduler core (LIFO slot
//! included) to another thread before the wait begins, so dispatch-to-delivery
//! stays in the millisecond range and the runtime keeps full parallelism.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aion::ActivityDispatcher;
use aion_core::{ActivityId, ContentType, Payload, WorkflowId};
use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
use dashmap::DashMap;

use super::dispatch::{ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink};
use super::heartbeat::{HeartbeatTracker, InFlightActivity};
use super::registry::{ConnectedWorkerRegistry, WorkerHandle, WorkerId, WorkerMessage};
use crate::error::ServerError;
use crate::shutdown::DrainState;
use tracing::info_span;

type SyncSender = std::sync::mpsc::SyncSender<Result<String, String>>;
type SyncReceiver = std::sync::mpsc::Receiver<Result<String, String>>;

/// Execution-scoped key for an in-flight activity dispatch.
///
/// Keying by bare [`ActivityId`] is unsafe across server restarts: the
/// dispatcher fabricates activity ids from a process-local counter
/// ([`WorkerActivityDispatcher::dispatch_blocking`]) that resets on restart,
/// so a stale result re-reported from a worker's previous session would
/// complete a *different* post-restart dispatch reusing the same sequence
/// position. The wire (`ActivityResult`) already carries both ids, and the
/// dispatcher fabricates a fresh `WorkflowId::new_v4()` per dispatch, so the
/// pair is collision-safe across restarts — a v4 uuid from the old server
/// life can never equal a fresh one.
///
/// The wire now carries an attempt discriminator (`ActivityTask.attempt`,
/// stamped from the engine-seam dispatch parameter), but the pending key
/// stays attempt-free: the dispatcher fabricates fresh ids per dispatch, so
/// two attempts of one logical activity are distinct `(workflow_id,
/// activity_id)` pairs here. When the engine passes *real* workflow ids,
/// redelivery bookkeeping can widen this key with the attempt it already has
/// on the wire — no further protocol change needed.
type PendingActivityKey = (WorkflowId, ActivityId);

/// Tracks in-flight activity dispatches waiting for worker results.
///
/// When the server's worker stream handler receives an `ActivityResult`, it
/// calls [`complete_activity`](ActivityCompletionSink::complete_activity) to
/// deliver the result to the blocked NIF thread. Entries are keyed by
/// [`PendingActivityKey`] so a stale result from a previous server life can
/// never be matched to a different execution (#59).
#[derive(Clone, Debug, Default)]
pub struct PendingActivities {
    pending: Arc<DashMap<PendingActivityKey, SyncSender>>,
}

impl PendingActivities {
    fn insert(&self, workflow_id: WorkflowId, activity_id: ActivityId) -> SyncReceiver {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.pending.insert((workflow_id, activity_id), tx);
        rx
    }

    fn complete(&self, key: &PendingActivityKey, result: Result<String, String>) -> bool {
        if let Some((_, sender)) = self.pending.remove(key) {
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
                payload_to_string(&payload).map_err(|reason| {
                    tracing::error!(
                        operation = "activity_complete",
                        workflow_id = %completion.workflow_id,
                        activity_id = %completion.activity_id,
                        error_type = "ActivityResultDecode",
                        %reason,
                        "activity completion failed"
                    );
                    ServerError::worker_dispatch("", "", format!("payload decode: {reason}"))
                })?
            }
            ActivityCompletionOutcome::Failed(error) => {
                let prefix = if error.is_retryable() {
                    "retryable"
                } else {
                    "terminal"
                };
                tracing::error!(
                    operation = "activity_complete",
                    workflow_id = %completion.workflow_id,
                    activity_id = %completion.activity_id,
                    error_type = "ActivityFailed",
                    error_kind = prefix,
                    reason = %error.message,
                    "activity completion failed"
                );
                Err(format!("{prefix}:{}", error.message))
            }
        };
        self.complete(&(completion.workflow_id, completion.activity_id), result);
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
/// Synchronous interface — uses `try_send` for the task channel and
/// `std::sync::mpsc::Receiver::recv_timeout` for the response. Callers on a
/// multi-thread tokio runtime are detected and moved into
/// `tokio::task::block_in_place` so the blocking wait never starves the
/// runtime tasks that flush the worker stream (see the module docs).
pub struct WorkerActivityDispatcher {
    registry: ConnectedWorkerRegistry,
    namespace: String,
    pending: PendingActivities,
    heartbeat_tracker: HeartbeatTracker,
    drain_state: DrainState,
    next_id: AtomicU64,
    timeout: Duration,
    workflow_registry: Option<Arc<aion::Registry>>,
    tokio_handle: Option<tokio::runtime::Handle>,
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
            workflow_registry: None,
            tokio_handle: None,
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

    /// Share the engine's active workflow registry for PID-to-handle correlation.
    #[must_use]
    pub fn with_workflow_registry(mut self, workflow_registry: Arc<aion::Registry>) -> Self {
        self.workflow_registry = Some(workflow_registry);
        self
    }

    /// Share the server runtime handle for sync history writes from dirty NIF threads.
    #[must_use]
    pub fn with_tokio_handle(mut self, tokio_handle: tokio::runtime::Handle) -> Self {
        self.tokio_handle = Some(tokio_handle);
        self
    }

    /// Override the per-activity dispatch timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl WorkerActivityDispatcher {
    fn ensure_accepting(
        &self,
        activity_type: &str,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        worker_id: Option<WorkerId>,
    ) -> Result<(), String> {
        self.drain_state
            .ensure_accepting(&self.namespace, activity_type)
            .map_err(|error| {
                let reason = error.to_string();
                log_worker_error(
                    "WorkerDispatch",
                    &self.namespace,
                    activity_type,
                    workflow_id,
                    activity_id,
                    worker_id,
                    &reason,
                );
                reason
            })
    }

    fn select_worker(
        &self,
        activity_type: &str,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<WorkerHandle, String> {
        self.registry
            .select_worker(&self.namespace, activity_type)
            .map_err(|error| {
                let reason = format!("registry error: {error}");
                log_worker_error(
                    "WorkerRegistry",
                    &self.namespace,
                    activity_type,
                    workflow_id,
                    activity_id,
                    None,
                    &reason,
                );
                reason
            })?
            .ok_or_else(|| {
                let reason = format!(
                    "no connected worker for activity type '{activity_type}' in namespace '{}'",
                    self.namespace
                );
                log_worker_error(
                    "WorkerUnavailable",
                    &self.namespace,
                    activity_type,
                    workflow_id,
                    activity_id,
                    None,
                    &reason,
                );
                reason
            })
    }

    fn track_worker_task(
        &self,
        worker_id: WorkerId,
        activity_type: &str,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<(), String> {
        self.heartbeat_tracker
            .track_task(
                worker_id,
                InFlightActivity {
                    workflow_id: workflow_id.clone(),
                    activity_id: activity_id.clone(),
                },
                Instant::now(),
            )
            .map_err(|error| {
                let reason = error.to_string();
                log_worker_error(
                    "WorkerHeartbeatTracker",
                    &self.namespace,
                    activity_type,
                    workflow_id,
                    activity_id,
                    Some(worker_id),
                    &reason,
                );
                reason
            })
    }

    fn cleanup_activity(
        &self,
        worker_id: WorkerId,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) {
        self.pending
            .pending
            .remove(&(workflow_id.clone(), activity_id.clone()));
        let _ = self
            .heartbeat_tracker
            .complete_task(worker_id, workflow_id, activity_id);
        self.drain_state.notify_activity_drained();
    }

    fn send_activity_task(
        &self,
        worker: &WorkerHandle,
        task: ProtoActivityTask,
        activity_type: &str,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<(), String> {
        match worker.sender().try_send(WorkerMessage::ActivityTask(task)) {
            Ok(()) => Ok(()),
            Err(error) => {
                let worker_id = worker.id();
                let reason = format!("worker task channel full or closed: {error}");
                self.cleanup_activity(worker_id, workflow_id, activity_id);
                log_worker_error(
                    "WorkerChannelClosed",
                    &self.namespace,
                    activity_type,
                    workflow_id,
                    activity_id,
                    Some(worker_id),
                    &reason,
                );
                Err(reason)
            }
        }
    }

    fn await_activity_result(
        &self,
        context: &ActivityDispatchContext<'_>,
        rx: &SyncReceiver,
    ) -> Result<String, String> {
        match rx.recv_timeout(self.timeout) {
            Ok(result) => {
                self.pending
                    .pending
                    .remove(&(context.workflow_id.clone(), context.activity_id.clone()));
                log_activity_completion(context, result.is_ok());
                result.inspect_err(|reason| {
                    log_worker_error(
                        "ActivityFailed",
                        &self.namespace,
                        context.activity_type,
                        context.workflow_id,
                        context.activity_id,
                        Some(context.worker_id),
                        reason,
                    );
                })
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.cleanup_activity(context.worker_id, context.workflow_id, context.activity_id);
                let reason = format!(
                    "activity '{}' timed out after {}s",
                    context.activity_type,
                    self.timeout.as_secs()
                );
                log_worker_error(
                    "ActivityTimeout",
                    &self.namespace,
                    context.activity_type,
                    context.workflow_id,
                    context.activity_id,
                    Some(context.worker_id),
                    &reason,
                );
                Err(reason)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.cleanup_activity(context.worker_id, context.workflow_id, context.activity_id);
                let reason = "activity response channel dropped".to_owned();
                log_worker_error(
                    "WorkerChannelClosed",
                    &self.namespace,
                    context.activity_type,
                    context.workflow_id,
                    context.activity_id,
                    Some(context.worker_id),
                    &reason,
                );
                Err(reason)
            }
        }
    }
}

impl ActivityDispatcher for WorkerActivityDispatcher {
    fn dispatch(
        &self,
        name: &str,
        input: &str,
        config: &str,
        attempt: u32,
    ) -> Result<String, String> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    // We are inside a tokio runtime (the engine spawns the
                    // sync dispatch onto its handle). Hand this worker's
                    // scheduler core to another thread before blocking so the
                    // stream forwarder woken by our `try_send` can actually
                    // run — otherwise it is trapped in this worker's
                    // non-stealable LIFO slot until the timeout fires.
                    tokio::task::block_in_place(|| {
                        self.dispatch_blocking(name, input, config, attempt)
                    })
                }
                flavor => Err(format!(
                    "activity dispatch blocks the calling thread until the worker responds; \
                     a {flavor:?} tokio runtime cannot host that wait because the worker \
                     stream forwarder shares its only executor thread and the task could \
                     never be delivered — run the engine on a multi-thread tokio runtime"
                )),
            },
            // No tokio context: a beamr scheduler thread or other plain OS
            // thread. Blocking here is the designed contract and cannot starve
            // the server runtime.
            Err(_) => self.dispatch_blocking(name, input, config, attempt),
        }
    }
}

impl WorkerActivityDispatcher {
    /// Dispatch the activity and block the calling thread until the worker
    /// responds or the timeout elapses.
    ///
    /// Must never run while the calling thread still owns a tokio scheduler
    /// core: the response can only arrive after the runtime's stream
    /// forwarder flushes the queued [`WorkerMessage::ActivityTask`] to the
    /// worker, so the thread blocking here must not be the one responsible
    /// for polling that forwarder. [`ActivityDispatcher::dispatch`] enforces
    /// this with `tokio::task::block_in_place`.
    fn dispatch_blocking(
        &self,
        name: &str,
        input: &str,
        config: &str,
        attempt: u32,
    ) -> Result<String, String> {
        let _ = config;
        let started_at = Instant::now();
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        let activity_id = ActivityId::from_sequence_position(sequence);
        let workflow_id = WorkflowId::new_v4();
        self.ensure_accepting(name, &workflow_id, &activity_id, None)?;
        let worker = self.select_worker(name, &workflow_id, &activity_id)?;
        let worker_id = worker.id();
        let span = info_span!(
            "activity_dispatch",
            operation = "activity_dispatch",
            namespace = %self.namespace,
            workflow_id = %workflow_id,
            activity_id = %activity_id,
            activity_type = %name,
            worker_id = ?worker_id,
        );
        let _span_guard = span.enter();
        self.ensure_accepting(name, &workflow_id, &activity_id, Some(worker_id))?;

        let task = activity_task(name, input, &workflow_id, &activity_id, attempt);
        let rx = self
            .pending
            .insert(workflow_id.clone(), activity_id.clone());
        self.track_worker_task(worker_id, name, &workflow_id, &activity_id)?;
        self.send_activity_task(&worker, task, name, &workflow_id, &activity_id)?;
        let context = ActivityDispatchContext {
            namespace: &self.namespace,
            activity_type: name,
            worker_id,
            workflow_id: &workflow_id,
            activity_id: &activity_id,
            started_at,
        };
        self.await_activity_result(&context, &rx)
    }
}

struct ActivityDispatchContext<'a> {
    namespace: &'a str,
    activity_type: &'a str,
    worker_id: WorkerId,
    workflow_id: &'a WorkflowId,
    activity_id: &'a ActivityId,
    started_at: Instant,
}

fn activity_task(
    activity_type: &str,
    input: &str,
    workflow_id: &WorkflowId,
    activity_id: &ActivityId,
    attempt: u32,
) -> ProtoActivityTask {
    ProtoActivityTask {
        workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
        activity_id: Some(ProtoActivityId::from(activity_id.clone())),
        activity_type: activity_type.to_owned(),
        input: Some(ProtoPayload {
            content_type: String::from("application/json"),
            bytes: input.as_bytes().to_vec(),
        }),
        attempt,
    }
}

fn log_activity_completion(context: &ActivityDispatchContext<'_>, succeeded: bool) {
    let duration_ms = duration_ms(context.started_at.elapsed());
    tracing::info!(
        operation = "activity_complete",
        namespace = context.namespace,
        workflow_id = %context.workflow_id,
        activity_id = %context.activity_id,
        activity_type = context.activity_type,
        worker_id = ?context.worker_id,
        duration_ms,
        outcome = if succeeded { "succeeded" } else { "failed" },
        "activity completed"
    );
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn log_worker_error(
    error_type: &'static str,
    namespace: &str,
    activity_type: &str,
    workflow_id: &WorkflowId,
    activity_id: &ActivityId,
    worker_id: Option<super::registry::WorkerId>,
    reason: &str,
) {
    tracing::error!(
        operation = "activity_dispatch",
        namespace,
        workflow_id = %workflow_id,
        activity_id = %activity_id,
        activity_type,
        worker_id = ?worker_id,
        error_type,
        reason,
        "worker interaction failed"
    );
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
        let workflow_id = WorkflowId::new_v4();
        let id = activity_id(1);
        let rx = pending.insert(workflow_id.clone(), id.clone());

        assert!(pending.complete(&(workflow_id, id), Ok("done".to_owned())));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)),
            Ok(Ok("done".to_owned()))
        );
    }

    #[test]
    fn pending_complete_unknown_returns_false() {
        let pending = PendingActivities::default();
        assert!(!pending.complete(
            &(WorkflowId::new_v4(), activity_id(99)),
            Ok("orphan".to_owned())
        ));
    }

    #[test]
    fn completion_sink_routes_success() -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        let workflow_id = WorkflowId::new_v4();
        let id = activity_id(2);
        let rx = pending.insert(workflow_id.clone(), id.clone());
        let payload = Payload::new(ContentType::Json, br#"{"greeting":"hi"}"#.to_vec());

        pending.complete_activity(ActivityCompletion {
            workflow_id,
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
        let workflow_id = WorkflowId::new_v4();
        let id = activity_id(3);
        let rx = pending.insert(workflow_id.clone(), id.clone());

        pending.complete_activity(ActivityCompletion {
            workflow_id,
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

    /// Regression test (#59, brief D12): pending tracking must be keyed by
    /// the full `(WorkflowId, ActivityId)` pair. The dispatcher fabricates
    /// activity ids from a process-local counter that resets on server
    /// restart, so a stale result re-reported from a worker's previous
    /// session carries the same bare `ActivityId` as a fresh post-restart
    /// dispatch. Under bare-`ActivityId` keying the stale result completed
    /// the wrong execution; with pair keying it is dropped and the genuine
    /// result still completes.
    #[test]
    fn stale_result_for_other_workflow_does_not_complete_pending_dispatch()
    -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        let post_restart_workflow = WorkflowId::new_v4();
        let pre_restart_workflow = WorkflowId::new_v4();
        // Counter resets to the same sequence position after restart.
        let id = activity_id(1);
        let rx = pending.insert(post_restart_workflow.clone(), id.clone());

        // Stale pre-restart result: same activity id, different workflow.
        pending.complete_activity(ActivityCompletion {
            workflow_id: pre_restart_workflow,
            activity_id: id.clone(),
            outcome: ActivityCompletionOutcome::Succeeded(Payload::new(
                ContentType::Json,
                br#""stale""#.to_vec(),
            )),
        })?;
        assert!(
            rx.try_recv().is_err(),
            "stale result for a different workflow must not complete this dispatch"
        );

        // The genuine result for the pending execution still completes.
        pending.complete_activity(ActivityCompletion {
            workflow_id: post_restart_workflow,
            activity_id: id,
            outcome: ActivityCompletionOutcome::Succeeded(Payload::new(
                ContentType::Json,
                br#""fresh""#.to_vec(),
            )),
        })?;
        let result = rx
            .recv_timeout(Duration::from_millis(50))
            .map_err(|e| ServerError::worker_dispatch("", "", format!("channel: {e}")))?;
        assert_eq!(result, Ok(r#""fresh""#.to_owned()));
        Ok(())
    }

    #[test]
    fn dispatcher_returns_error_when_no_worker_registered() {
        let registry = ConnectedWorkerRegistry::default();
        let dispatcher = WorkerActivityDispatcher::new(registry, "default");

        let result = dispatcher.dispatch("greet", "{}", "{}", 1);

        assert!(result.is_err());
        let err = result.err().unwrap_or_default();
        assert!(
            err.contains("no connected worker"),
            "unexpected error: {err}"
        );
    }

    /// Regression test for the production stall where every remote activity
    /// timed out: the engine invoked the sync `dispatch` from inside a
    /// spawned tokio task (`futures::future::lazy` polled on a runtime
    /// worker), and the woken stream-consumer task landed in that blocked
    /// worker's non-stealable LIFO slot, so the queued `ActivityTask` was
    /// only delivered when the timeout fired.
    ///
    /// Mirrors the real wiring minus tonic: the real registry channel that
    /// the gRPC stream forwarder drains, a worker task awaiting that channel
    /// on the same runtime, completion through the production
    /// `ActivityCompletionSink`, and the sync dispatch invoked from a
    /// runtime worker task — the worst case the `block_in_place` guard in
    /// `dispatch` defends against (the engine itself now routes through
    /// `dispatch_async_from_process`, off the async workers).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_inside_runtime_task_delivers_promptly_and_round_trips()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let pending = PendingActivities::default();
        let (worker_tx, mut worker_rx) = tokio::sync::mpsc::channel(32);
        let activity_types = [String::from("greet")];
        let registration = registry.register("default", activity_types.iter(), worker_tx)?;

        let sink = pending.clone();
        let echo_worker = tokio::spawn(async move {
            let Some(WorkerMessage::ActivityTask(task)) = worker_rx.recv().await else {
                return Err("expected an activity task on the worker channel".to_owned());
            };
            let workflow_id = task
                .workflow_id
                .ok_or("task missing workflow id")
                .and_then(|id| WorkflowId::try_from(id).map_err(|_| "bad workflow id"))?;
            let activity_id = task
                .activity_id
                .map(ActivityId::from)
                .ok_or("task missing activity id")?;
            sink.complete_activity(ActivityCompletion {
                workflow_id,
                activity_id,
                outcome: ActivityCompletionOutcome::Succeeded(Payload::new(
                    ContentType::Json,
                    br#"{"greeting":"hello"}"#.to_vec(),
                )),
            })
            .map_err(|error| error.to_string())
        });

        let dispatcher =
            Arc::new(WorkerActivityDispatcher::new(registry, "default").with_pending(pending));
        let started = Instant::now();
        // Invoke the sync dispatch inside the first poll of a spawned task:
        // the worst-case calling context for the `block_in_place` guard.
        let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
            dispatcher.dispatch("greet", "{}", "{}", 1)
        }));
        let result = dispatch_task.await.map_err(|error| error.to_string())?;
        let elapsed = started.elapsed();

        assert_eq!(result, Ok(r#"{"greeting":"hello"}"#.to_owned()));
        assert!(
            elapsed < Duration::from_secs(5),
            "dispatch round trip took {elapsed:?}; task delivery must not be \
             coupled to the dispatch timeout"
        );
        echo_worker.await.map_err(|error| error.to_string())??;
        registration.deregister()?;
        Ok(())
    }

    /// A current-thread runtime cannot host the blocking wait (the stream
    /// forwarder would share its only executor thread), so dispatch must
    /// fail fast with a precise error instead of stalling until the timeout.
    #[tokio::test]
    async fn dispatch_on_current_thread_runtime_fails_fast()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let (worker_tx, _worker_rx) = tokio::sync::mpsc::channel(32);
        let activity_types = [String::from("greet")];
        let registration = registry.register("default", activity_types.iter(), worker_tx)?;
        let dispatcher = WorkerActivityDispatcher::new(registry, "default");

        let started = Instant::now();
        let result = dispatcher.dispatch("greet", "{}", "{}", 1);
        let elapsed = started.elapsed();

        let err = result.err().ok_or("expected dispatch to fail")?;
        assert!(
            err.contains("multi-thread tokio runtime"),
            "unexpected error: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "fail-fast path took {elapsed:?}"
        );
        registration.deregister()?;
        Ok(())
    }
}
