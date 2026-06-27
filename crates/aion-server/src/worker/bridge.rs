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
//! blocks on `std::sync::mpsc::Receiver::recv`.
//!
//! Blocking is harmless on a beamr thread, but on a tokio runtime worker it
//! must be wrapped in `tokio::task::block_in_place`: the `try_send` wakes the
//! per-worker gRPC stream forwarder task, and tokio schedules a task woken
//! from task context into the *current* worker's LIFO slot, which no other
//! runtime worker can steal. Without the `block_in_place` core handoff the
//! forwarder sits trapped in that slot while this thread blocks, so the queued
//! `ActivityTask` is never flushed to the worker even though the worker is
//! healthy. `block_in_place` moves the worker's scheduler core (LIFO slot
//! included) to another thread before the wait begins, so dispatch-to-delivery
//! stays in the millisecond range and the runtime keeps full parallelism.
//!
//! # Wait termination
//!
//! The engine imposes no activity timeout of its own: agent-style activities
//! legitimately run for over an hour, so the completion wait is unbounded.
//! The blocking `recv` terminates on exactly one of:
//!
//! - **Completion** — the worker reports a result and the stream handler
//!   delivers it through [`ActivityCompletionSink::complete_activity`].
//! - **Worker loss** — the worker's gRPC stream ends (process death,
//!   disconnect, expired token); the stream teardown sweeps the worker's
//!   in-flight tasks through the same sink as retryable lost-worker
//!   failures ([`HeartbeatTracker::fail_disconnected_worker`]).
//! - **Drain timeout at shutdown** — the shutdown coordinator fails all
//!   remaining in-flight tasks through the sink
//!   (`HeartbeatTracker::fail_all_in_flight_workers`).
//! - **Channel teardown** — every sender for the pending entry is dropped
//!   (a cleanup path removed the entry without completing it); surfaced as
//!   a channel-closed dispatch error, never a hang.
//!
//! An activity's duration is bounded only by the workflow's own
//! `timeout_seconds` and by worker liveness — never by an engine constant.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use aion::{ActivityDispatch, ActivityDispatcher};
use aion_core::{ActivityId, ContentType, Payload, RunId, WorkflowId};
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
/// The engine seam ([`ActivityDispatch`]) carries the *real* workflow id and
/// the *real* per-workflow activity ordinal recorded in history, so this pair
/// uniquely and stably identifies one execution. Keying by bare [`ActivityId`]
/// would be unsafe across server restarts — a stale result re-reported from a
/// worker's previous session could complete a *different* post-restart
/// dispatch reusing the same ordinal — but pairing it with the real workflow
/// id closes that race: two different workflow executions never share a
/// workflow id, so a stale `(workflow_id, activity_id)` from a previous server
/// life can only ever match the exact execution it belongs to.
///
/// The wire (`ActivityResult`) carries both ids, plus an attempt discriminator
/// (`ActivityTask.attempt`). The pending key stays attempt-free for now: a
/// retry re-dispatches under the same `(workflow_id, activity_id)` and the
/// outstanding entry is the one awaiting completion. Redelivery bookkeeping
/// can widen this key with the wire attempt later — no protocol change needed.
type PendingActivityKey = (WorkflowId, ActivityId);

/// Routes an unmatched durable-outbox completion into the live workflow.
///
/// When the outbox is ON a worker completion can arrive at the sink with no
/// pending oneshot (the dispatch was non-blocking fan-out, or the original
/// waiter was lost). Rather than dropping it, [`PendingActivities::complete`]
/// hands it to this callback, which resolves the workflow to its live engine
/// process and delivers the terminal into its mailbox. The callback is only
/// installed when the outbox is enabled, so flag-off the unmatched branch
/// stays a silent drop.
pub trait OutboxDeliveryCallback: Send + Sync {
    /// Deliver a successful completion to the live workflow.
    ///
    /// Returns `Ok(true)` when delivered to a live workflow and `Ok(false)`
    /// when no run is currently live (the expected stale-completion case that
    /// recovery re-arms).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the engine rejects the delivery.
    fn deliver_completion(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        result: String,
    ) -> Result<bool, ServerError>;

    /// Deliver a failure to the live workflow. Same `bool`/error contract as
    /// [`Self::deliver_completion`].
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the engine rejects the delivery.
    fn deliver_failure(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        reason: String,
    ) -> Result<bool, ServerError>;
}

/// Tracks in-flight activity dispatches waiting for worker results.
///
/// When the server's worker stream handler receives an `ActivityResult`, it
/// calls [`complete_activity`](ActivityCompletionSink::complete_activity) to
/// deliver the result to the blocked NIF thread. Entries are keyed by
/// [`PendingActivityKey`] so a stale result from a previous server life can
/// never be matched to a different execution (#59).
///
/// Clones share both the pending map and the outbox-delivery callback through
/// `Arc`, so [`set_outbox_delivery`](Self::set_outbox_delivery) called once on
/// any clone after construction is visible to the clone the dispatcher holds.
#[derive(Clone, Default)]
pub struct PendingActivities {
    pending: Arc<DashMap<PendingActivityKey, SyncSender>>,
    outbox_delivery: Arc<OnceLock<Arc<dyn OutboxDeliveryCallback>>>,
}

impl std::fmt::Debug for PendingActivities {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingActivities")
            .field("pending", &self.pending.len())
            .field(
                "outbox_delivery_installed",
                &self.outbox_delivery.get().is_some(),
            )
            .finish()
    }
}

impl PendingActivities {
    fn insert(&self, workflow_id: WorkflowId, activity_id: ActivityId) -> SyncReceiver {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.pending.insert((workflow_id, activity_id), tx);
        rx
    }

    /// Install the unmatched-completion delivery callback (idempotent).
    ///
    /// Set once, after construction, when the durable outbox is enabled. A
    /// second set is ignored and logged: the callback is process-wide and must
    /// not silently change identity.
    pub fn set_outbox_delivery(&self, callback: Arc<dyn OutboxDeliveryCallback>) {
        if self.outbox_delivery.set(callback).is_err() {
            tracing::warn!("outbox delivery callback already installed; ignoring duplicate set");
        }
    }

    /// Complete a pending dispatch, or route an unmatched completion to the
    /// outbox delivery callback when one is installed.
    ///
    /// A matched entry delivers to its waiting oneshot exactly as before. An
    /// unmatched completion is dropped silently when no callback is installed
    /// (outbox OFF — byte-identical to the prior behaviour); with a callback
    /// installed (outbox ON) it is routed into the live workflow's mailbox.
    fn complete(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        result: Result<String, String>,
    ) -> bool {
        // Take and drop the DashMap guard before any callback runs: the engine
        // delivery the callback invokes must never execute under a shard lock.
        let matched = self
            .pending
            .remove(&(workflow_id.clone(), activity_id.clone()));
        if let Some((_, sender)) = matched {
            return sender.send(result).is_ok();
        }
        let Some(callback) = self.outbox_delivery.get() else {
            // Outbox OFF: silent drop, byte-identical to the prior behaviour.
            return false;
        };
        let outcome = match result {
            Ok(payload) => callback.deliver_completion(workflow_id, activity_id, run_id, payload),
            Err(reason) => callback.deliver_failure(workflow_id, activity_id, run_id, reason),
        };
        match outcome {
            Ok(true) => true,
            Ok(false) => {
                // Not live: the expected stale-completion case recovery re-arms.
                tracing::debug!(
                    workflow_id = %workflow_id,
                    activity_id = %activity_id,
                    "unmatched outbox completion for a workflow that is not currently live; \
                     recovery will re-arm it"
                );
                false
            }
            Err(error) => {
                tracing::warn!(
                    workflow_id = %workflow_id,
                    activity_id = %activity_id,
                    %error,
                    "failed to deliver unmatched outbox completion to the live workflow"
                );
                false
            }
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
        self.complete(
            &completion.workflow_id,
            &completion.activity_id,
            completion.run_id.as_ref(),
            result,
        );
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
/// `std::sync::mpsc::Receiver::recv` for the response. Callers on a
/// multi-thread tokio runtime are detected and moved into
/// `tokio::task::block_in_place` so the blocking wait never starves the
/// runtime tasks that flush the worker stream (see the module docs).
pub struct WorkerActivityDispatcher {
    registry: ConnectedWorkerRegistry,
    namespace: String,
    pending: PendingActivities,
    heartbeat_tracker: HeartbeatTracker,
    drain_state: DrainState,
    tokio_handle: Option<tokio::runtime::Handle>,
}

impl std::fmt::Debug for WorkerActivityDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerActivityDispatcher")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl WorkerActivityDispatcher {
    /// Build a dispatcher for the given namespace, worker registry, and
    /// liveness tracker.
    ///
    /// The tracker must be the same instance the worker stream handler and
    /// shutdown coordinator share: the unbounded completion wait relies on
    /// stream teardown sweeping this tracker's in-flight entries to fail
    /// dispatches whose worker was lost.
    #[must_use]
    pub fn new(
        registry: ConnectedWorkerRegistry,
        namespace: impl Into<String>,
        heartbeat_tracker: HeartbeatTracker,
    ) -> Self {
        Self {
            registry,
            namespace: namespace.into(),
            pending: PendingActivities::default(),
            heartbeat_tracker,
            drain_state: DrainState::default(),
            tokio_handle: None,
        }
    }

    /// Share a caller-supplied pending-activities tracker.
    #[must_use]
    pub fn with_pending(mut self, pending: PendingActivities) -> Self {
        self.pending = pending;
        self
    }

    /// Share the server drain gate.
    #[must_use]
    pub fn with_drain_state(mut self, drain_state: DrainState) -> Self {
        self.drain_state = drain_state;
        self
    }

    /// Share the server runtime handle for sync history writes from dirty NIF threads.
    #[must_use]
    pub fn with_tokio_handle(mut self, tokio_handle: tokio::runtime::Handle) -> Self {
        self.tokio_handle = Some(tokio_handle);
        self
    }
}

impl WorkerActivityDispatcher {
    fn ensure_accepting(
        &self,
        namespace: &str,
        activity_type: &str,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        worker_id: Option<WorkerId>,
    ) -> Result<(), String> {
        self.drain_state
            .ensure_accepting(namespace, activity_type)
            .map_err(|error| {
                let reason = error.to_string();
                log_worker_error(
                    "WorkerDispatch",
                    namespace,
                    activity_type,
                    workflow_id,
                    activity_id,
                    worker_id,
                    &reason,
                );
                reason
            })
    }

    /// Select a worker for the namespace and activity type, waiting if none is
    /// currently available. Blocks until a matching worker registers or the
    /// server begins draining.
    fn select_worker_or_wait(
        &self,
        namespace: &str,
        task_queue: &str,
        activity_type: &str,
        node: Option<&str>,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<WorkerHandle, String> {
        loop {
            // `node` is the OPTIONAL within-pool affinity carried on the
            // dispatch: `Some(n)` pins selection to workers advertising node
            // `n` (require semantics — it waits, via the no-worker path below,
            // if none are present); `None` is unpinned and reaches any worker
            // in the (namespace, task_queue) pool — byte-identical to the
            // pre-NODE behaviour.
            match self
                .registry
                .select_worker(namespace, task_queue, activity_type, node)
            {
                Ok(Some(worker)) => return Ok(worker),
                Ok(None) => {
                    self.ensure_accepting(
                        namespace,
                        activity_type,
                        workflow_id,
                        activity_id,
                        None,
                    )?;
                    tracing::info!(
                        namespace,
                        activity_type,
                        node,
                        workflow_id = %workflow_id,
                        activity_id = %activity_id,
                        "no connected worker; waiting for a matching worker to register"
                    );
                    match &self.tokio_handle {
                        Some(handle) => {
                            handle.block_on(self.registry.wait_for_worker());
                        }
                        None => match tokio::runtime::Handle::try_current() {
                            Ok(handle) => {
                                handle.block_on(self.registry.wait_for_worker());
                            }
                            Err(_) => {
                                std::thread::sleep(Duration::from_millis(500));
                            }
                        },
                    }
                }
                Err(error) => {
                    let reason = format!("registry error: {error}");
                    log_worker_error(
                        "WorkerRegistry",
                        namespace,
                        activity_type,
                        workflow_id,
                        activity_id,
                        None,
                        &reason,
                    );
                    return Err(reason);
                }
            }
        }
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

    /// Block until the dispatch terminates (see the module docs for the
    /// exhaustive termination list). The wait is deliberately unbounded:
    /// the engine imposes no activity timeout of its own.
    fn await_activity_result(
        &self,
        context: &ActivityDispatchContext<'_>,
        rx: &SyncReceiver,
    ) -> Result<String, String> {
        // Close the dispatch/disconnect race before blocking. A worker whose
        // stream tore down *before* this dispatch tracked its task was swept
        // without this entry, so nothing would ever deliver through `rx`.
        // `fail_lost_worker` deregisters before it collects tasks, and this
        // dispatch tracked its task before sending, so: if the worker is
        // still registered here, any later sweep is guaranteed to include
        // this task and unblock the `recv` below.
        match self.registry.is_registered(context.worker_id) {
            Ok(true) => {}
            Ok(false) => {
                // A sweep that did include this task may have delivered
                // already; prefer its verdict (or a genuine result that
                // raced the disconnect) over fabricating one.
                if let Ok(result) = rx.try_recv() {
                    return self.deliver_result(context, result);
                }
                self.cleanup_activity(context.worker_id, context.workflow_id, context.activity_id);
                let reason = format!(
                    "retryable:{}",
                    super::dispatch::lost_worker_error(context.worker_id).message
                );
                log_worker_error(
                    "WorkerLost",
                    &self.namespace,
                    context.activity_type,
                    context.workflow_id,
                    context.activity_id,
                    Some(context.worker_id),
                    &reason,
                );
                return Err(reason);
            }
            Err(error) => {
                self.cleanup_activity(context.worker_id, context.workflow_id, context.activity_id);
                let reason = format!("worker registry inspection failed: {error}");
                log_worker_error(
                    "WorkerRegistry",
                    &self.namespace,
                    context.activity_type,
                    context.workflow_id,
                    context.activity_id,
                    Some(context.worker_id),
                    &reason,
                );
                return Err(reason);
            }
        }
        if let Ok(result) = rx.recv() {
            return self.deliver_result(context, result);
        }
        // Every sender was dropped without completing: a cleanup path
        // removed the pending entry. Surface it instead of hanging.
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

    fn deliver_result(
        &self,
        context: &ActivityDispatchContext<'_>,
        result: Result<String, String>,
    ) -> Result<String, String> {
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
}

impl ActivityDispatcher for WorkerActivityDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    // We are inside a tokio runtime (the engine spawns the
                    // sync dispatch onto its handle). Hand this worker's
                    // scheduler core to another thread before blocking so the
                    // stream forwarder woken by our `try_send` can actually
                    // run — otherwise it is trapped in this worker's
                    // non-stealable LIFO slot for as long as we block.
                    tokio::task::block_in_place(|| self.dispatch_blocking(request))
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
            Err(_) => self.dispatch_blocking(request),
        }
    }
}

impl WorkerActivityDispatcher {
    /// Dispatch the activity and block the calling thread until the worker
    /// responds, the worker is declared lost, or the server drains (see the
    /// module docs for the exhaustive termination list).
    ///
    /// The request carries the *real* workflow and activity ids the engine
    /// recorded in history, so the worker logs, the pending-completion key,
    /// and the heartbeat tracker all correlate directly against the event
    /// store. `config` is forwarded by the engine seam but not yet consumed
    /// here (the retry executor that reads it is unbuilt).
    ///
    /// Must never run while the calling thread still owns a tokio scheduler
    /// core: the response can only arrive after the runtime's stream
    /// forwarder flushes the queued [`WorkerMessage::ActivityTask`] to the
    /// worker, so the thread blocking here must not be the one responsible
    /// for polling that forwarder. [`ActivityDispatcher::dispatch`] enforces
    /// this with `tokio::task::block_in_place`.
    fn dispatch_blocking(&self, request: ActivityDispatch) -> Result<String, String> {
        let ActivityDispatch {
            namespace,
            task_queue,
            // OPTIONAL within-pool node affinity (NODE-4): `Some(n)` pins this
            // dispatch to workers advertising node `n` (require semantics);
            // `None` is unpinned and reaches any worker in the pool.
            node,
            workflow_id,
            activity_id,
            name,
            input,
            config: _,
            attempt,
            labels,
        } = request;
        let started_at = Instant::now();
        self.ensure_accepting(&namespace, &name, &workflow_id, &activity_id, None)?;
        let worker = self.select_worker_or_wait(
            &namespace,
            &task_queue,
            &name,
            node.as_deref(),
            &workflow_id,
            &activity_id,
        )?;
        let worker_id = worker.id();
        let span = info_span!(
            "activity_dispatch",
            operation = "activity_dispatch",
            namespace = %namespace,
            task_queue = %task_queue,
            node = node.as_deref(),
            workflow_id = %workflow_id,
            activity_id = %activity_id,
            activity_type = %name,
            worker_id = ?worker_id,
        );
        let _span_guard = span.enter();
        self.ensure_accepting(
            &namespace,
            &name,
            &workflow_id,
            &activity_id,
            Some(worker_id),
        )?;

        let task = activity_task(&name, &input, &workflow_id, &activity_id, attempt, labels);
        let rx = self
            .pending
            .insert(workflow_id.clone(), activity_id.clone());
        self.track_worker_task(worker_id, &name, &workflow_id, &activity_id)?;
        self.send_activity_task(&worker, task, &name, &workflow_id, &activity_id)?;
        let context = ActivityDispatchContext {
            namespace: &namespace,
            activity_type: &name,
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
    labels: BTreeMap<String, String>,
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
        labels: labels.into_iter().collect(),
        // The synchronous `ActivityDispatch` bridge path carries no run context
        // (run scoping is threaded through the durable-outbox path; OBX-011).
        run_id: None,
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
    use std::sync::Mutex;

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

        assert!(pending.complete(&workflow_id, &id, None, Ok("done".to_owned())));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)),
            Ok(Ok("done".to_owned()))
        );
    }

    #[test]
    fn pending_complete_unknown_returns_false() {
        let pending = PendingActivities::default();
        assert!(!pending.complete(
            &WorkflowId::new_v4(),
            &activity_id(99),
            None,
            Ok("orphan".to_owned())
        ));
    }

    #[derive(Default)]
    struct RecordingOutboxCallback {
        completions: Mutex<Vec<(WorkflowId, ActivityId, String)>>,
        failures: Mutex<Vec<(WorkflowId, ActivityId, String)>>,
        live: bool,
    }

    impl OutboxDeliveryCallback for RecordingOutboxCallback {
        fn deliver_completion(
            &self,
            workflow_id: &WorkflowId,
            activity_id: &ActivityId,
            run_id: Option<&RunId>,
            result: String,
        ) -> Result<bool, ServerError> {
            let _ = run_id;
            self.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording outbox callback"))?
                .push((workflow_id.clone(), activity_id.clone(), result));
            Ok(self.live)
        }

        fn deliver_failure(
            &self,
            workflow_id: &WorkflowId,
            activity_id: &ActivityId,
            run_id: Option<&RunId>,
            reason: String,
        ) -> Result<bool, ServerError> {
            let _ = run_id;
            self.failures
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording outbox callback"))?
                .push((workflow_id.clone(), activity_id.clone(), reason));
            Ok(self.live)
        }
    }

    #[test]
    fn unmatched_completion_routes_to_outbox_callback_when_installed() -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        let callback = Arc::new(RecordingOutboxCallback {
            live: true,
            ..RecordingOutboxCallback::default()
        });
        // Install on one clone; the wiring must be visible to every clone.
        pending.clone().set_outbox_delivery(callback.clone());

        let workflow_id = WorkflowId::new_v4();
        let id = activity_id(7);

        // No pending entry: the completion is unmatched and must route to the
        // callback rather than being dropped. A live workflow reports true.
        assert!(pending.complete(&workflow_id, &id, None, Ok("done".to_owned())));
        let completions = callback
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording outbox callback"))?;
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].0, workflow_id);
        assert_eq!(completions[0].1, id);
        assert_eq!(completions[0].2, "done");
        Ok(())
    }

    #[test]
    fn unmatched_failure_routes_to_outbox_callback_and_not_live_reports_false()
    -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        // live = false models the expected stale-completion case.
        let callback = Arc::new(RecordingOutboxCallback::default());
        pending.set_outbox_delivery(callback.clone());

        let workflow_id = WorkflowId::new_v4();
        let id = activity_id(8);

        assert!(!pending.complete(&workflow_id, &id, None, Err("retryable:boom".to_owned())));
        let failures = callback
            .failures
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording outbox callback"))?;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].2, "retryable:boom");
        Ok(())
    }

    #[test]
    fn unmatched_completion_is_silent_drop_when_no_callback_installed() {
        // Flag-off byte-identical behaviour: no callback, unmatched returns
        // false (silent drop) exactly as before.
        let pending = PendingActivities::default();
        assert!(!pending.complete(
            &WorkflowId::new_v4(),
            &activity_id(9),
            None,
            Ok("x".to_owned())
        ));
    }

    #[test]
    fn matched_completion_never_reaches_outbox_callback() -> Result<(), ServerError> {
        let pending = PendingActivities::default();
        let callback = Arc::new(RecordingOutboxCallback {
            live: true,
            ..RecordingOutboxCallback::default()
        });
        pending.set_outbox_delivery(callback.clone());

        let workflow_id = WorkflowId::new_v4();
        let id = activity_id(10);
        let rx = pending.insert(workflow_id.clone(), id.clone());

        assert!(pending.complete(&workflow_id, &id, None, Ok("matched".to_owned())));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)),
            Ok(Ok("matched".to_owned()))
        );
        assert!(
            callback
                .completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording outbox callback"))?
                .is_empty(),
            "a matched completion must deliver to its waiter, not the outbox callback"
        );
        Ok(())
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
            run_id: None,
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
            run_id: None,
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
            run_id: None,
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
            run_id: None,
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

    /// Liveness tracker for dispatcher unit tests; the window only matters
    /// to expiry checks, which nothing in these tests drives.
    fn test_tracker() -> HeartbeatTracker {
        HeartbeatTracker::new(Duration::from_secs(5))
    }

    /// A `greet` dispatch request carrying real (test-synthesized) ids, the
    /// engine-seam shape `WorkerActivityDispatcher::dispatch` now consumes.
    fn greet_request() -> ActivityDispatch {
        ActivityDispatch {
            namespace: "default".to_owned(),
            task_queue: "default".to_owned(),
            node: None,
            workflow_id: WorkflowId::new_v4(),
            activity_id: ActivityId::from_sequence_position(0),
            name: "greet".to_owned(),
            input: "{}".to_owned(),
            config: "{}".to_owned(),
            attempt: 1,
            labels: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn dispatcher_fails_immediately_when_draining_without_workers() {
        let registry = ConnectedWorkerRegistry::default();
        let drain = DrainState::default();
        let dispatcher = WorkerActivityDispatcher::new(registry, "default", test_tracker())
            .with_drain_state(drain.clone());

        let _ = drain.begin();

        let result = dispatcher.dispatch(greet_request());

        assert!(result.is_err());
        let err = result.err().unwrap_or_default();
        assert!(
            err.contains("drain"),
            "expected drain rejection, got: {err}"
        );
    }

    /// Regression test for the production stall where every remote activity
    /// timed out: the engine invoked the sync `dispatch` from inside a
    /// spawned tokio task (`futures::future::lazy` polled on a runtime
    /// worker), and the woken stream-consumer task landed in that blocked
    /// worker's non-stealable LIFO slot, so the queued `ActivityTask` was
    /// only delivered when the then-extant 30s dispatch timeout fired (the
    /// dispatch wait is unbounded today; the stall would now be a hang).
    ///
    /// Mirrors the real wiring minus tonic: the real registry channel that
    /// the gRPC stream forwarder drains, a worker task awaiting that channel
    /// on the same runtime, completion through the production
    /// `ActivityCompletionSink`, and the sync dispatch invoked from a
    /// runtime worker task — the worst case the `block_in_place` guard in
    /// `dispatch` defends against (the engine itself now routes through
    /// `dispatch_async`, off the async workers).
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
                run_id: None,
                outcome: ActivityCompletionOutcome::Succeeded(Payload::new(
                    ContentType::Json,
                    br#"{"greeting":"hello"}"#.to_vec(),
                )),
            })
            .map_err(|error| error.to_string())
        });

        let dispatcher = Arc::new(
            WorkerActivityDispatcher::new(registry, "default", test_tracker())
                .with_pending(pending),
        );
        let started = Instant::now();
        // Invoke the sync dispatch inside the first poll of a spawned task:
        // the worst-case calling context for the `block_in_place` guard.
        let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
            dispatcher.dispatch(greet_request())
        }));
        let result = dispatch_task.await.map_err(|error| error.to_string())?;
        let elapsed = started.elapsed();

        assert_eq!(result, Ok(r#"{"greeting":"hello"}"#.to_owned()));
        assert!(
            elapsed < Duration::from_secs(5),
            "dispatch round trip took {elapsed:?}; task delivery must not \
             depend on the blocked dispatch thread"
        );
        echo_worker.await.map_err(|error| error.to_string())??;
        registration.deregister()?;
        Ok(())
    }

    /// A current-thread runtime cannot host the blocking wait (the stream
    /// forwarder would share its only executor thread), so dispatch must
    /// fail fast with a precise error instead of blocking forever.
    #[tokio::test]
    async fn dispatch_on_current_thread_runtime_fails_fast()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let (worker_tx, _worker_rx) = tokio::sync::mpsc::channel(32);
        let activity_types = [String::from("greet")];
        let registration = registry.register("default", activity_types.iter(), worker_tx)?;
        let dispatcher = WorkerActivityDispatcher::new(registry, "default", test_tracker());

        let started = Instant::now();
        let result = dispatcher.dispatch(greet_request());
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

    /// Bridge-level mirror of the e2e node-pin proof: two workers share the
    /// `(namespace, task_queue)` pool but advertise different nodes; an
    /// `ActivityDispatch` pinned to one node must reach ONLY the worker on that
    /// node through the live engine-seam `WorkerActivityDispatcher`. This is the
    /// regression guard for the bridge discarding the dispatch's `node`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_pinned_to_node_reaches_only_that_node()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let pending = PendingActivities::default();
        let activity_types = [String::from("greet")];
        let (n1_tx, mut n1_rx) = tokio::sync::mpsc::channel(32);
        let (n2_tx, mut n2_rx) = tokio::sync::mpsc::channel(32);
        // Register the DECOY (n2) FIRST so it owns the lowest worker id. The
        // bridge's `select_worker` picks the lowest-id matching worker, so a
        // bridge that DISCARDED the node would route to n2 (the decoy) here —
        // the n1 echo would never fire and the round trip would time out. With
        // the node threaded through, selection is filtered to n1.
        let on_n2 = registry.register_namespaces(
            [String::from("default")],
            "default",
            Some(String::from("n2")),
            activity_types.iter(),
            n2_tx,
        )?;
        let on_n1 = registry.register_namespaces(
            [String::from("default")],
            "default",
            Some(String::from("n1")),
            activity_types.iter(),
            n1_tx,
        )?;

        // Echo only on the n1 channel: the dispatch can only complete if the
        // task was routed to n1. If it leaked to n2, the n1 wait would stall and
        // the round trip below would time out instead.
        let sink = pending.clone();
        let echo_n1 = tokio::spawn(async move {
            let Some(WorkerMessage::ActivityTask(task)) = n1_rx.recv().await else {
                return Err("expected an activity task on the n1 worker channel".to_owned());
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
                run_id: None,
                outcome: ActivityCompletionOutcome::Succeeded(Payload::new(
                    ContentType::Json,
                    br#"{"greeting":"hello"}"#.to_vec(),
                )),
            })
            .map_err(|error| error.to_string())
        });

        let dispatcher = Arc::new(
            WorkerActivityDispatcher::new(registry.clone(), "default", test_tracker())
                .with_pending(pending),
        );

        let pinned = ActivityDispatch {
            node: Some(String::from("n1")),
            ..greet_request()
        };
        let started = Instant::now();
        let result = tokio::spawn(futures::future::lazy(move |_| dispatcher.dispatch(pinned)))
            .await
            .map_err(|error| error.to_string())?;
        let elapsed = started.elapsed();

        assert_eq!(result, Ok(r#"{"greeting":"hello"}"#.to_owned()));
        assert!(
            elapsed < Duration::from_secs(5),
            "pinned dispatch round trip took {elapsed:?}; the task must route to n1"
        );
        echo_n1.await.map_err(|error| error.to_string())??;

        // The n2 worker (wrong node) must never have been handed the task.
        assert!(
            n2_rx.try_recv().is_err(),
            "node=Some(\"n1\") dispatch must not reach the n2 worker"
        );

        on_n1.deregister()?;
        on_n2.deregister()?;
        Ok(())
    }

    /// An unpinned (`node = None`) dispatch is byte-identical to today: it
    /// reaches a worker in the pool regardless of the worker's advertised node.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unpinned_dispatch_reaches_a_pooled_worker_regardless_of_node()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let pending = PendingActivities::default();
        let activity_types = [String::from("greet")];
        let (n1_tx, mut n1_rx) = tokio::sync::mpsc::channel(32);
        let on_n1 = registry.register_namespaces(
            [String::from("default")],
            "default",
            Some(String::from("n1")),
            activity_types.iter(),
            n1_tx,
        )?;

        let sink = pending.clone();
        let echo = tokio::spawn(async move {
            let Some(WorkerMessage::ActivityTask(task)) = n1_rx.recv().await else {
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
                run_id: None,
                outcome: ActivityCompletionOutcome::Succeeded(Payload::new(
                    ContentType::Json,
                    br#"{"greeting":"hello"}"#.to_vec(),
                )),
            })
            .map_err(|error| error.to_string())
        });

        let dispatcher = Arc::new(
            WorkerActivityDispatcher::new(registry.clone(), "default", test_tracker())
                .with_pending(pending),
        );

        // greet_request() carries node: None — the unpinned path.
        let result = tokio::spawn(futures::future::lazy(move |_| {
            dispatcher.dispatch(greet_request())
        }))
        .await
        .map_err(|error| error.to_string())?;

        assert_eq!(result, Ok(r#"{"greeting":"hello"}"#.to_owned()));
        echo.await.map_err(|error| error.to_string())??;
        on_n1.deregister()?;
        Ok(())
    }
}
