//! Heartbeat window tracking and lost-worker failure surfacing.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};
use tracing::{error, info, warn};

use aion_core::{ActivityId, Payload, WorkflowId};
use aion_proto::{ProtoHeartbeat, WireError};

use crate::error::ServerError;
use crate::shutdown::DrainState;
use crate::worker::dispatch::{
    ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink, lost_worker_error,
};
use crate::worker::registry::{ConnectedWorkerRegistry, WorkerId};

/// In-flight activity assigned to a connected worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InFlightActivity {
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Correlating activity id.
    pub activity_id: ActivityId,
}

/// Observable liveness state for a single in-flight activity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLiveness {
    /// Worker currently responsible for the task.
    pub worker_id: WorkerId,
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Correlating activity id.
    pub activity_id: ActivityId,
    /// Operator-configured heartbeat window used for expiry checks.
    pub heartbeat_window: Duration,
    /// Monotonic timestamp of assignment or the most recent heartbeat.
    pub last_heartbeat_at: Instant,
    /// Optional worker progress from the most recent heartbeat.
    pub last_progress: Option<Payload>,
}

/// Result of accepting a heartbeat for an in-flight task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeartbeatUpdate {
    /// Updated liveness after recording the heartbeat.
    pub liveness: TaskLiveness,
}

/// Tasks removed from tracking because a worker was declared lost.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LostWorkerReport {
    /// Lost worker removed from the connected-worker registry.
    pub worker_id: WorkerId,
    /// In-flight activities swept off the tracker: surfaced to the engine as
    /// retryable failures on the `fail_*` paths, or parked for restart
    /// recovery (nothing recorded, nothing delivered) on the graceful-drain
    /// `park_*` paths (#207).
    pub tasks: Vec<InFlightActivity>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TaskKey(WorkerId, WorkflowId, ActivityId);

#[derive(Debug, Default)]
struct HeartbeatState {
    tasks: HashMap<TaskKey, TaskLiveness>,
}

/// Per-task liveness tracker for remote-worker streams.
#[derive(Clone, Debug)]
pub struct HeartbeatTracker {
    heartbeat_window: Duration,
    inner: Arc<Mutex<HeartbeatState>>,
    empty: Arc<Notify>,
}

impl HeartbeatTracker {
    /// Build a tracker using the operator-supplied heartbeat window.
    #[must_use]
    pub fn new(heartbeat_window: Duration) -> Self {
        Self {
            heartbeat_window,
            inner: Arc::new(Mutex::new(HeartbeatState::default())),
            empty: Arc::new(Notify::new()),
        }
    }

    /// Track a newly accepted in-flight activity for heartbeat expiry.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if tracker state cannot be trusted.
    pub fn track_task(
        &self,
        worker_id: WorkerId,
        task: InFlightActivity,
        now: Instant,
    ) -> Result<(), ServerError> {
        let key = TaskKey::new(
            worker_id,
            task.workflow_id.clone(),
            task.activity_id.clone(),
        );
        let liveness = TaskLiveness {
            worker_id,
            workflow_id: task.workflow_id,
            activity_id: task.activity_id,
            heartbeat_window: self.heartbeat_window,
            last_heartbeat_at: now,
            last_progress: None,
        };
        self.state()?.tasks.insert(key, liveness);
        Ok(())
    }

    /// Stop tracking a completed activity and wake drain waiters if this was the last task.
    ///
    /// Returns whether the task was still tracked when this ran: `true` means
    /// THIS call retired the in-flight entry, `false` means another path (the
    /// expiry sweep, a disconnect teardown, shutdown, or a completed dispatch)
    /// already did. The liminal reply router uses that bool as its structural
    /// gate for synthesizing a lost-worker failure — the exact mirror of the
    /// gRPC sweep failing only still-tracked tasks.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if tracker state cannot be trusted.
    pub fn complete_task(
        &self,
        worker_id: WorkerId,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<bool, ServerError> {
        let key = TaskKey::new(worker_id, workflow_id.clone(), activity_id.clone());
        let (was_tracked, became_empty) = {
            let mut state = self.state()?;
            let was_tracked = state.tasks.remove(&key).is_some();
            (was_tracked, state.tasks.is_empty())
        };
        if became_empty {
            self.empty.notify_waiters();
        }
        Ok(was_tracked)
    }

    /// Whether the given in-flight task is still tracked (not yet completed,
    /// swept, or drained). The liminal reply router polls this to bound its
    /// wait: once the entry is gone the dispatch was resolved by another path,
    /// so the router exits instead of parking on the connection forever.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if tracker state cannot be trusted.
    pub fn is_tracked(
        &self,
        worker_id: WorkerId,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<bool, ServerError> {
        let key = TaskKey::new(worker_id, workflow_id.clone(), activity_id.clone());
        Ok(self.state()?.tasks.contains_key(&key))
    }

    /// Refresh the liveness stamp of an in-flight task from a transport-level
    /// liveness beat that carries no progress payload (the liminal worker's
    /// automatic pump). Returns `true` when the task was tracked and refreshed,
    /// `false` when it is not in flight — a benign outcome for a beat racing a
    /// completion or covering an outbox dispatch the tracker never held.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if tracker state cannot be trusted.
    pub fn record_liveness(
        &self,
        worker_id: WorkerId,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        now: Instant,
    ) -> Result<bool, ServerError> {
        let key = TaskKey::new(worker_id, workflow_id.clone(), activity_id.clone());
        let mut state = self.state()?;
        let Some(liveness) = state.tasks.get_mut(&key) else {
            return Ok(false);
        };
        liveness.last_heartbeat_at = now;
        Ok(true)
    }

    /// The operator-configured heartbeat window this tracker expires against.
    /// The bridge stamps it onto each liminal dispatch so the worker's
    /// automatic liveness pump beats at the matching quarter-window cadence.
    #[must_use]
    pub const fn heartbeat_window(&self) -> Duration {
        self.heartbeat_window
    }

    /// Number of currently tracked in-flight activities.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if tracker state cannot be trusted.
    pub fn in_flight_count(&self) -> Result<usize, ServerError> {
        Ok(self.state()?.tasks.len())
    }

    /// Record a worker heartbeat without completing the activity.
    ///
    /// Every heartbeat refreshes the task's liveness stamp. The progress
    /// payload is only overwritten when the heartbeat CARRIES one: the worker
    /// runtime's automatic liveness beats are payload-free and interleave
    /// with explicit handler progress heartbeats, and a liveness beat must
    /// never erase the handler's most recent progress report.
    ///
    /// # Errors
    ///
    /// Returns a stable wire error for malformed heartbeats or unknown in-flight tasks.
    pub fn record_heartbeat(
        &self,
        worker_id: WorkerId,
        heartbeat: ProtoHeartbeat,
        now: Instant,
    ) -> Result<HeartbeatUpdate, ServerError> {
        let decoded = DecodedHeartbeat::try_from(heartbeat)?;
        let key = TaskKey::new(worker_id, decoded.workflow_id, decoded.activity_id);
        let mut state = self.state()?;
        let Some(liveness) = state.tasks.get_mut(&key) else {
            return Err(wire_error("heartbeat task is not in flight"));
        };
        liveness.last_heartbeat_at = now;
        if decoded.progress.is_some() {
            liveness.last_progress = decoded.progress;
        }
        Ok(HeartbeatUpdate {
            liveness: liveness.clone(),
        })
    }

    /// Return whether an in-flight task is still within its configured heartbeat window.
    ///
    /// # Errors
    ///
    /// Returns a stable wire error if the task is not tracked, or lock poison if state cannot be trusted.
    pub fn is_live(
        &self,
        worker_id: WorkerId,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        now: Instant,
    ) -> Result<bool, ServerError> {
        let key = TaskKey::new(worker_id, workflow_id.clone(), activity_id.clone());
        let state = self.state()?;
        let Some(liveness) = state.tasks.get(&key) else {
            return Err(wire_error("heartbeat task is not in flight"));
        };
        Ok(!is_expired(liveness, now))
    }

    /// Return the workers that have at least one task beyond the configured heartbeat window.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if tracker state cannot be trusted.
    pub fn expired_workers(&self, now: Instant) -> Result<Vec<WorkerId>, ServerError> {
        let state = self.state()?;
        let mut seen = HashSet::new();
        let mut workers = Vec::new();
        for liveness in state.tasks.values() {
            if is_expired(liveness, now) && seen.insert(liveness.worker_id) {
                workers.push(liveness.worker_id);
            }
        }
        workers.sort_unstable();
        Ok(workers)
    }

    /// Mark all currently expired workers lost and fail their in-flight tasks through the engine sink.
    ///
    /// # Errors
    ///
    /// Returns registry, tracker, or sink errors without retrying or rescheduling activities.
    pub fn fail_expired_workers(
        &self,
        registry: &ConnectedWorkerRegistry,
        sink: &impl ActivityCompletionSink,
        now: Instant,
    ) -> Result<Vec<LostWorkerReport>, ServerError> {
        let mut reports = Vec::new();
        for worker_id in self.expired_workers(now)? {
            let report = self.fail_lost_worker(worker_id, registry, sink)?;
            if !report.tasks.is_empty() {
                reports.push(report);
            }
        }
        Ok(reports)
    }

    /// Mark a disconnected worker lost and fail its in-flight tasks through the engine sink.
    ///
    /// # Errors
    ///
    /// Returns registry, tracker, or sink errors without retrying or rescheduling activities.
    pub fn fail_disconnected_worker(
        &self,
        worker_id: WorkerId,
        registry: &ConnectedWorkerRegistry,
        sink: &impl ActivityCompletionSink,
    ) -> Result<LostWorkerReport, ServerError> {
        self.fail_lost_worker(worker_id, registry, sink)
    }

    /// Mark every currently in-flight worker lost and fail all remaining tasks through the sink.
    ///
    /// # Errors
    ///
    /// Returns registry, tracker, or sink errors without retrying or rescheduling activities.
    pub fn fail_all_in_flight_workers(
        &self,
        registry: &ConnectedWorkerRegistry,
        sink: &impl ActivityCompletionSink,
    ) -> Result<Vec<LostWorkerReport>, ServerError> {
        let worker_ids = {
            let state = self.state()?;
            let mut worker_ids = state
                .tasks
                .values()
                .map(|liveness| liveness.worker_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            worker_ids.sort_unstable();
            worker_ids
        };
        let mut reports = Vec::new();
        for worker_id in worker_ids {
            let report = self.fail_lost_worker(worker_id, registry, sink)?;
            if !report.tasks.is_empty() {
                reports.push(report);
            }
        }
        self.empty.notify_waiters();
        Ok(reports)
    }

    /// Park a drain-disconnected worker's in-flight tasks for restart recovery
    /// (#207): deregister the worker, remove its tracked tasks, and resolve
    /// each pending waiter through [`ActivityCompletionSink::park_activity`].
    ///
    /// The graceful-drain counterpart of [`Self::fail_disconnected_worker`]:
    /// same deregister-before-collect ordering (same closed dispatch/disconnect
    /// race), but NO completion is synthesized — the durable log keeps its
    /// dangling scheduled/started trail, byte-equivalent to a kill -9, and
    /// restart recovery re-dispatches it. Deregistered with the honest
    /// [`WorkerDeathReason::Disconnect`](aion_core::WorkerDeathReason::Disconnect):
    /// the transport genuinely dropped (the worker obeyed the drain request).
    ///
    /// # Errors
    ///
    /// Returns registry, tracker, or sink errors without retrying or rescheduling activities.
    pub fn park_disconnected_worker(
        &self,
        worker_id: WorkerId,
        registry: &ConnectedWorkerRegistry,
        sink: &impl ActivityCompletionSink,
    ) -> Result<LostWorkerReport, ServerError> {
        self.park_lost_worker(
            worker_id,
            registry,
            sink,
            aion_core::WorkerDeathReason::Disconnect,
        )
    }

    /// Park EVERY currently in-flight worker's tasks for restart recovery
    /// (#207) — the drain-timeout backstop's bulk counterpart of
    /// [`Self::fail_all_in_flight_workers`].
    ///
    /// Deregistered with the honest
    /// [`WorkerDeathReason::Timeout`](aion_core::WorkerDeathReason::Timeout):
    /// the drain window genuinely expired on these workers. Wakes drain waiters
    /// after the sweep so `wait_for_empty` observes the emptied tracker.
    ///
    /// # Errors
    ///
    /// Returns registry, tracker, or sink errors without retrying or rescheduling activities.
    pub fn park_all_in_flight_workers(
        &self,
        registry: &ConnectedWorkerRegistry,
        sink: &impl ActivityCompletionSink,
    ) -> Result<Vec<LostWorkerReport>, ServerError> {
        let worker_ids = {
            let state = self.state()?;
            let mut worker_ids = state
                .tasks
                .values()
                .map(|liveness| liveness.worker_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            worker_ids.sort_unstable();
            worker_ids
        };
        let mut reports = Vec::new();
        for worker_id in worker_ids {
            let report = self.park_lost_worker(
                worker_id,
                registry,
                sink,
                aion_core::WorkerDeathReason::Timeout,
            )?;
            if !report.tasks.is_empty() {
                reports.push(report);
            }
        }
        self.empty.notify_waiters();
        Ok(reports)
    }

    /// Shared park core (#207), structured exactly like [`Self::fail_lost_worker`]
    /// — deregister BEFORE collecting tasks (see that method's race note) — but
    /// resolving each waiter with the ephemeral parked sentinel instead of
    /// synthesizing a lost-worker `ActivityFailed`. Idempotent for the same
    /// reasons: `deregister_with_reason` no-ops on an already-removed worker and
    /// each task is removed as it parks, so a second sweep (park or fail) sees
    /// an empty report and resolves nothing.
    fn park_lost_worker(
        &self,
        worker_id: WorkerId,
        registry: &ConnectedWorkerRegistry,
        sink: &impl ActivityCompletionSink,
        reason: aion_core::WorkerDeathReason,
    ) -> Result<LostWorkerReport, ServerError> {
        registry.deregister_with_reason(worker_id, reason)?;
        let tasks = self.remove_worker_tasks(worker_id)?;
        for task in &tasks {
            sink.park_activity(&task.workflow_id, &task.activity_id)?;
            info!(
                worker_id = ?worker_id,
                workflow_id = %task.workflow_id,
                activity_id = %task.activity_id,
                "activity parked for restart recovery"
            );
        }
        Ok(LostWorkerReport { worker_id, tasks })
    }

    fn fail_lost_worker(
        &self,
        worker_id: WorkerId,
        registry: &ConnectedWorkerRegistry,
        sink: &impl ActivityCompletionSink,
    ) -> Result<LostWorkerReport, ServerError> {
        // Deregister BEFORE collecting tasks: the dispatch path tracks its
        // task, sends, and then checks `registry.is_registered`. With this
        // ordering, a dispatch that still sees the worker registered is
        // guaranteed its tracked task is visible to any later sweep, so the
        // unbounded completion wait always gets a lost-worker failure. (The
        // reverse order leaves a window where a task tracked between the
        // collection and the deregistration is never failed by anyone.)
        // This is the liveness-timeout sweep: the proven reason is Timeout, the
        // one finer-grained WS3 distinction this call site can honestly assert.
        registry.deregister_with_reason(worker_id, aion_core::WorkerDeathReason::Timeout)?;
        let tasks = self.remove_worker_tasks(worker_id)?;
        for task in &tasks {
            sink.complete_activity(ActivityCompletion {
                workflow_id: task.workflow_id.clone(),
                activity_id: task.activity_id.clone(),
                run_id: None,
                outcome: ActivityCompletionOutcome::Failed(lost_worker_error(worker_id)),
            })?;
        }
        Ok(LostWorkerReport { worker_id, tasks })
    }

    fn remove_worker_tasks(
        &self,
        worker_id: WorkerId,
    ) -> Result<Vec<InFlightActivity>, ServerError> {
        let mut state = self.state()?;
        let keys = state
            .tasks
            .keys()
            .filter(|key| key.worker_id() == worker_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut tasks = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(liveness) = state.tasks.remove(&key) {
                tasks.push(InFlightActivity {
                    workflow_id: liveness.workflow_id,
                    activity_id: liveness.activity_id,
                });
            }
        }
        Ok(tasks)
    }

    fn state(&self) -> Result<MutexGuard<'_, HeartbeatState>, ServerError> {
        self.inner
            .lock()
            .map_err(|_| ServerError::lock_poisoned("worker heartbeat tracker"))
    }
}

/// Sweep cadence derived from the operator's `worker.heartbeat_window`: a
/// quarter of the window, clamped to `[1s, window]` (the default 30s window
/// sweeps every 7.5s).
///
/// Deliberately derived rather than a separate config knob: the window is the
/// operational contract ("a silent worker is dead after this long"), and the
/// sweep cadence is an implementation detail of enforcing it — a quarter-window
/// cadence bounds detection latency at `window + window/4` while keeping the
/// sweep cheap. A window shorter than one second (test configurations) sweeps
/// once per window rather than sub-second-spinning, and a zero window is
/// floored at one millisecond because `tokio::time::interval` rejects a zero
/// period.
#[must_use]
pub fn sweep_interval(heartbeat_window: Duration) -> Duration {
    /// `tokio::time::interval` panics on a zero period, so even a
    /// (misconfigured) zero window gets a positive cadence.
    const MINIMUM_PERIOD: Duration = Duration::from_millis(1);
    /// Target lower bound: sweeping more often than once a second buys no
    /// meaningful detection latency against real heartbeat windows.
    const TARGET_FLOOR: Duration = Duration::from_secs(1);
    let ceiling = heartbeat_window.max(MINIMUM_PERIOD);
    // The floor never exceeds the ceiling, so `clamp` cannot panic.
    (heartbeat_window / 4).clamp(TARGET_FLOOR.min(ceiling), ceiling)
}

/// Production driver of [`HeartbeatTracker::fail_expired_workers`] (#176).
///
/// The tracker records per-task liveness, and the stream-teardown sweep fails a
/// worker whose stream ENDS — but a worker whose stream stays open while its
/// process wedges (stops heartbeating without disconnecting) was never expired
/// by anything on the boot path, so its in-flight dispatches waited forever.
/// This interval task is that missing caller: each tick fails every worker with
/// a task beyond its heartbeat window, deregistering it with the provable
/// [`WorkerDeathReason::Timeout`](aion_core::WorkerDeathReason::Timeout) and
/// surfacing its tasks as retryable lost-worker failures through the shared
/// completion sink. It shares the server's shutdown watch, so it drains with
/// the transports (mirroring
/// [`OutboxDispatcher::run`](crate::worker::OutboxDispatcher::run)).
///
/// Double-fail safety: this sweep and the stream-teardown path
/// ([`HeartbeatTracker::fail_disconnected_worker`]) can both observe the same
/// dead worker. Both funnel into the same idempotent core —
/// `deregister_with_reason` is a no-op for an already-removed worker (no
/// duplicate WS3 delta, no metrics double-count) and the tracker removes each
/// task as it fails it — so whichever path runs second sees an empty report and
/// never double-completes an activity.
pub struct HeartbeatSweeper<S> {
    tracker: HeartbeatTracker,
    registry: ConnectedWorkerRegistry,
    sink: S,
    drain: DrainState,
    heartbeat_window: Duration,
    interval: Duration,
}

impl<S> HeartbeatSweeper<S>
where
    S: ActivityCompletionSink + Send + Sync + 'static,
{
    /// Build a sweeper over the server's shared liveness tracker, worker
    /// registry, completion sink, and drain gate. The cadence is derived from
    /// `heartbeat_window` by [`sweep_interval`].
    #[must_use]
    pub fn new(
        tracker: HeartbeatTracker,
        registry: ConnectedWorkerRegistry,
        sink: S,
        drain: DrainState,
        heartbeat_window: Duration,
    ) -> Self {
        let interval = sweep_interval(heartbeat_window);
        Self {
            tracker,
            registry,
            sink,
            drain,
            heartbeat_window,
            interval,
        }
    }

    /// Run the expiry sweep until `shutdown` flips to `true`.
    ///
    /// A tracker/registry error during a sweep is logged and retried next tick
    /// rather than tearing the task down — a transient failure must not
    /// silently stop dead-worker detection. Shutdown is observed both while
    /// waiting for the next tick and re-checked before each sweep, exactly like
    /// the outbox dispatcher's run loop.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        info!(
            sweep_interval_ms = self.interval.as_millis(),
            heartbeat_window_ms = self.heartbeat_window.as_millis(),
            "worker heartbeat sweeper started"
        );
        let mut ticks = tokio::time::interval(self.interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticks.tick() => {
                    if *shutdown.borrow() {
                        break;
                    }
                    self.sweep_once(Instant::now());
                }
                changed = shutdown.changed() => {
                    // A receive error means every sender dropped; treat that as
                    // a shutdown request rather than spinning.
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        info!("worker heartbeat sweeper stopped");
    }

    /// Fail every currently-expired worker once, logging each lost-worker
    /// report at warn (mirroring the stream-teardown sweep's logging).
    fn sweep_once(&self, now: Instant) {
        let reports = match self
            .tracker
            .fail_expired_workers(&self.registry, &self.sink, now)
        {
            Ok(reports) => reports,
            Err(sweep_error) => {
                error!(
                    error = %sweep_error,
                    "heartbeat expiry sweep failed; retrying next tick"
                );
                return;
            }
        };
        for report in &reports {
            warn!(
                worker_id = ?report.worker_id,
                failed_tasks = report.tasks.len(),
                "worker heartbeat window expired with in-flight activities; \
                 deregistered and surfaced as retryable lost-worker failures"
            );
        }
        if !reports.is_empty() {
            // In-flight accounting may have just reached zero; wake any drain
            // waiter so shutdown does not sit out its full timeout (mirrors
            // the stream-teardown sweep).
            self.drain.notify_activity_drained();
        }
    }
}

impl TaskKey {
    fn new(worker_id: WorkerId, workflow_id: WorkflowId, activity_id: ActivityId) -> Self {
        Self(worker_id, workflow_id, activity_id)
    }

    const fn worker_id(&self) -> WorkerId {
        self.0
    }
}

struct DecodedHeartbeat {
    workflow_id: WorkflowId,
    activity_id: ActivityId,
    progress: Option<Payload>,
}

impl TryFrom<ProtoHeartbeat> for DecodedHeartbeat {
    type Error = ServerError;

    fn try_from(value: ProtoHeartbeat) -> Result<Self, Self::Error> {
        let workflow_id = value
            .workflow_id
            .ok_or_else(|| wire_error("heartbeat workflow id is missing"))
            .and_then(|id| WorkflowId::try_from(id).map_err(ServerError::from))?;
        let activity_id = value
            .activity_id
            .ok_or_else(|| wire_error("heartbeat activity id is missing"))
            .map(ActivityId::from)?;
        let progress = value
            .progress
            .map(Payload::try_from)
            .transpose()
            .map_err(ServerError::from)?;
        Ok(Self {
            workflow_id,
            activity_id,
            progress,
        })
    }
}

fn is_expired(liveness: &TaskLiveness, now: Instant) -> bool {
    now.checked_duration_since(liveness.last_heartbeat_at)
        .is_some_and(|elapsed| elapsed > liveness.heartbeat_window)
}

fn wire_error(message: &'static str) -> ServerError {
    ServerError::Wire {
        wire: WireError::backend(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use aion_core::{ActivityErrorKind, ContentType};
    use aion_proto::{ProtoActivityId, ProtoPayload, ProtoWorkflowId};
    use serde_json::json;
    use uuid::Uuid;

    use crate::worker::registry::WorkerRegistration;

    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        completions: Mutex<Vec<ActivityCompletion>>,
        parks: Mutex<Vec<(WorkflowId, ActivityId)>>,
    }

    impl ActivityCompletionSink for RecordingSink {
        fn complete_activity(&self, completion: ActivityCompletion) -> Result<(), ServerError> {
            self.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .push(completion);
            Ok(())
        }

        fn park_activity(
            &self,
            workflow_id: &WorkflowId,
            activity_id: &ActivityId,
        ) -> Result<(), ServerError> {
            self.parks
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .push((workflow_id.clone(), activity_id.clone()));
            Ok(())
        }
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(Uuid::nil())
    }

    fn activity_id(position: u64) -> ActivityId {
        ActivityId::from_sequence_position(position)
    }

    fn payload(value: &serde_json::Value) -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(value)?)
    }

    fn heartbeat(
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        progress: Option<Payload>,
    ) -> ProtoHeartbeat {
        ProtoHeartbeat {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
            progress: progress.map(ProtoPayload::from),
        }
    }

    fn registry_with_worker()
    -> Result<(ConnectedWorkerRegistry, WorkerRegistration, WorkerId), ServerError> {
        let registry = ConnectedWorkerRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let activity_types = [String::from("charge-card")];
        let registration = registry.register("tenant-a", activity_types.iter(), tx)?;
        let worker_id = registration
            .worker_id()
            .ok_or_else(|| ServerError::lock_poisoned("test worker registration"))?;
        Ok((registry, registration, worker_id))
    }

    #[test]
    fn heartbeat_refresh_keeps_task_live_across_window() -> Result<(), Box<dyn std::error::Error>> {
        let window = Duration::from_secs(5);
        let tracker = HeartbeatTracker::new(window);
        let worker_id = WorkerIdForTest::registered()?;
        let workflow_id = workflow_id();
        let activity_id = activity_id(10);
        let start = Instant::now();

        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
            },
            start,
        )?;
        assert!(tracker.is_live(worker_id, &workflow_id, &activity_id, start + window)?);

        let progress = payload(&json!({"percent": 50}))?;
        let update = tracker.record_heartbeat(
            worker_id,
            heartbeat(
                workflow_id.clone(),
                activity_id.clone(),
                Some(progress.clone()),
            ),
            start + window,
        )?;

        assert_eq!(update.liveness.last_progress, Some(progress));
        assert!(tracker.is_live(
            worker_id,
            &workflow_id,
            &activity_id,
            start + window + window
        )?);
        assert!(tracker.expired_workers(start + window + window)?.is_empty());
        Ok(())
    }

    #[test]
    fn missed_heartbeat_deregisters_worker_and_fails_in_flight_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let (registry, _registration, worker_id) = registry_with_worker()?;
        let sink = RecordingSink::default();
        let tracker = HeartbeatTracker::new(Duration::from_secs(5));
        let workflow_id = workflow_id();
        let activity_id = activity_id(11);
        let start = Instant::now();

        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
            },
            start,
        )?;

        let reports =
            tracker.fail_expired_workers(&registry, &sink, start + Duration::from_secs(6))?;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].worker_id, worker_id);
        assert_eq!(reports[0].tasks.len(), 1);
        assert!(
            registry
                .workers_for("tenant-a", "default", "charge-card", None)?
                .is_empty()
        );

        let second = tracker.fail_disconnected_worker(worker_id, &registry, &sink)?;
        assert!(second.tasks.is_empty());
        let completions = sink
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].workflow_id, workflow_id);
        assert_eq!(completions[0].activity_id, activity_id);
        match &completions[0].outcome {
            ActivityCompletionOutcome::Failed(error) => {
                assert_eq!(error.kind, ActivityErrorKind::Retryable);
                assert!(error.is_retryable());
            }
            ActivityCompletionOutcome::Succeeded(_) => {
                return Err("expected lost-worker failure".into());
            }
        }
        Ok(())
    }

    #[test]
    fn disconnected_worker_fails_each_in_flight_task_once() -> Result<(), Box<dyn std::error::Error>>
    {
        let (registry, _registration, worker_id) = registry_with_worker()?;
        let sink = RecordingSink::default();
        let tracker = HeartbeatTracker::new(Duration::from_secs(5));
        let workflow_id = workflow_id();
        let start = Instant::now();

        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id(21),
            },
            start,
        )?;
        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id,
                activity_id: activity_id(22),
            },
            start,
        )?;

        let report = tracker.fail_disconnected_worker(worker_id, &registry, &sink)?;
        assert_eq!(report.tasks.len(), 2);
        assert!(
            registry
                .workers_for("tenant-a", "default", "charge-card", None)?
                .is_empty()
        );

        let completions = sink
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;
        assert_eq!(completions.len(), 2);
        assert!(completions.iter().all(|completion| matches!(
            &completion.outcome,
            ActivityCompletionOutcome::Failed(error)
                if error.kind == ActivityErrorKind::Retryable && error.is_retryable()
        )));
        Ok(())
    }

    /// #207: parking a drain-disconnected worker removes its tasks, deregisters
    /// it, and PARKS each task through the sink — zero completions synthesized,
    /// so the durable log stays byte-equivalent to a kill -9. A second park (or
    /// a later fail sweep) finds nothing: the idempotent-deregister discipline
    /// the fail path already proves holds for parks too.
    #[test]
    fn park_disconnected_worker_parks_tasks_without_synthesizing_completions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (registry, _registration, worker_id) = registry_with_worker()?;
        let sink = RecordingSink::default();
        let tracker = HeartbeatTracker::new(Duration::from_secs(5));
        let workflow_id = workflow_id();
        let start = Instant::now();
        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id(60),
            },
            start,
        )?;
        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id(61),
            },
            start,
        )?;

        let report = tracker.park_disconnected_worker(worker_id, &registry, &sink)?;
        assert_eq!(report.tasks.len(), 2);
        assert_eq!(
            tracker.in_flight_count()?,
            0,
            "parking must remove every tracked task so drain accounting reaches zero"
        );
        assert!(
            registry
                .workers_for("tenant-a", "default", "charge-card", None)?
                .is_empty(),
            "the parked worker must be deregistered from routing"
        );
        let parks = sink
            .parks
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;
        assert_eq!(parks.len(), 2, "each task must be parked exactly once");
        drop(parks);
        assert!(
            sink.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .is_empty(),
            "parking must never synthesize an activity completion"
        );

        // Double-park and park-after-fail are no-ops: the idempotent core.
        let second = tracker.park_disconnected_worker(worker_id, &registry, &sink)?;
        assert!(second.tasks.is_empty());
        let third = tracker.fail_disconnected_worker(worker_id, &registry, &sink)?;
        assert!(third.tasks.is_empty());
        assert_eq!(
            sink.parks
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .len(),
            2,
            "re-sweeping a parked worker must park nothing further"
        );
        assert!(
            sink.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .is_empty(),
            "a fail sweep after the park must fail nothing"
        );
        Ok(())
    }

    /// #207 drain-timeout backstop: the bulk park removes every worker's tasks,
    /// parks each through the sink, and wakes drain waiters — never
    /// synthesizing a completion.
    #[tokio::test]
    async fn park_all_in_flight_workers_parks_everything_and_wakes_drain_waiters()
    -> Result<(), Box<dyn std::error::Error>> {
        let (registry, _registration, worker_id) = registry_with_worker()?;
        let sink = RecordingSink::default();
        let tracker = HeartbeatTracker::new(Duration::from_secs(5));
        let workflow_id = workflow_id();
        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id(70),
            },
            Instant::now(),
        )?;
        // Arm a waiter on the tracker's empty notify BEFORE the bulk park.
        let notified = tracker.empty.notified();
        tokio::pin!(notified);

        let reports = tracker.park_all_in_flight_workers(&registry, &sink)?;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].worker_id, worker_id);
        assert_eq!(reports[0].tasks.len(), 1);
        assert_eq!(tracker.in_flight_count()?, 0);
        assert_eq!(
            sink.parks
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .len(),
            1
        );
        assert!(
            sink.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .is_empty(),
            "the bulk park must never synthesize a completion"
        );
        tokio::time::timeout(Duration::from_millis(200), notified)
            .await
            .map_err(|_| "the bulk park must wake drain waiters")?;
        Ok(())
    }

    /// The worker runtime's AUTOMATIC liveness beats carry no payload and
    /// interleave with explicit handler progress heartbeats: a payload-free
    /// beat must refresh the liveness stamp WITHOUT erasing the handler's
    /// most recent progress report.
    #[test]
    fn payload_free_heartbeat_refreshes_liveness_without_clearing_progress()
    -> Result<(), Box<dyn std::error::Error>> {
        let window = Duration::from_secs(5);
        let tracker = HeartbeatTracker::new(window);
        let worker_id = WorkerIdForTest::registered()?;
        let workflow_id = workflow_id();
        let activity_id = activity_id(12);
        let start = Instant::now();

        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
            },
            start,
        )?;
        let progress = payload(&json!({"percent": 80}))?;
        tracker.record_heartbeat(
            worker_id,
            heartbeat(
                workflow_id.clone(),
                activity_id.clone(),
                Some(progress.clone()),
            ),
            start + Duration::from_secs(1),
        )?;

        // An automatic liveness beat: no payload, later timestamp.
        let update = tracker.record_heartbeat(
            worker_id,
            heartbeat(workflow_id.clone(), activity_id.clone(), None),
            start + Duration::from_secs(4),
        )?;

        assert_eq!(
            update.liveness.last_progress,
            Some(progress),
            "a payload-free liveness beat must not erase handler progress"
        );
        assert!(
            tracker.is_live(
                worker_id,
                &workflow_id,
                &activity_id,
                start + Duration::from_secs(8)
            )?,
            "the payload-free beat must still refresh the liveness stamp"
        );
        Ok(())
    }

    #[test]
    fn malformed_heartbeat_missing_ids_is_wire_error() -> Result<(), Box<dyn std::error::Error>> {
        let worker_id = WorkerIdForTest::registered()?;
        let tracker = HeartbeatTracker::new(Duration::from_secs(5));
        let missing = ProtoHeartbeat {
            workflow_id: None,
            activity_id: Some(ProtoActivityId::from(activity_id(30))),
            progress: None,
        };

        let result = tracker.record_heartbeat(worker_id, missing, Instant::now());
        assert!(matches!(result, Err(ServerError::Wire { .. })));
        Ok(())
    }

    #[test]
    fn heartbeat_progress_is_not_reported_as_activity_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let sink = RecordingSink::default();
        let worker_id = WorkerIdForTest::registered()?;
        let tracker = HeartbeatTracker::new(Duration::from_secs(5));
        let workflow_id = workflow_id();
        let activity_id = activity_id(40);
        let now = Instant::now();

        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
            },
            now,
        )?;
        tracker.record_heartbeat(
            worker_id,
            heartbeat(
                workflow_id,
                activity_id,
                Some(Payload::new(
                    ContentType::Json,
                    b"{\"progress\":1}".to_vec(),
                )),
            ),
            now,
        )?;

        let completions = sink
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;
        assert!(completions.is_empty());
        Ok(())
    }

    struct WorkerIdForTest;

    impl WorkerIdForTest {
        fn registered() -> Result<WorkerId, ServerError> {
            let (_registry, _registration, worker_id) = registry_with_worker()?;
            Ok(worker_id)
        }
    }

    /// `complete_task` reports whether THIS call retired the entry — the
    /// structural gate the liminal reply router uses to synthesize a
    /// lost-worker failure only for a dispatch nobody else resolved.
    #[test]
    fn complete_task_reports_whether_the_entry_was_tracked()
    -> Result<(), Box<dyn std::error::Error>> {
        let tracker = HeartbeatTracker::new(Duration::from_secs(5));
        let worker_id = WorkerIdForTest::registered()?;
        let workflow_id = workflow_id();
        let id = activity_id(50);
        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: id.clone(),
            },
            Instant::now(),
        )?;

        assert!(tracker.is_tracked(worker_id, &workflow_id, &id)?);
        assert!(
            tracker.complete_task(worker_id, &workflow_id, &id)?,
            "the first completion retires the tracked entry"
        );
        assert!(!tracker.is_tracked(worker_id, &workflow_id, &id)?);
        assert!(
            !tracker.complete_task(worker_id, &workflow_id, &id)?,
            "a second completion finds nothing to retire"
        );
        Ok(())
    }

    /// A liveness beat (the liminal worker's automatic pump) refreshes the
    /// task's expiry stamp — keeping a genuinely-running over-window activity
    /// out of the sweep — and reports an untracked task benignly.
    #[test]
    fn record_liveness_refreshes_stamp_and_ignores_untracked_tasks()
    -> Result<(), Box<dyn std::error::Error>> {
        let window = Duration::from_secs(5);
        let tracker = HeartbeatTracker::new(window);
        let worker_id = WorkerIdForTest::registered()?;
        let workflow_id = workflow_id();
        let id = activity_id(51);
        let start = Instant::now();
        tracker.track_task(
            worker_id,
            InFlightActivity {
                workflow_id: workflow_id.clone(),
                activity_id: id.clone(),
            },
            start,
        )?;

        // Beaten at the window edge, the task survives past the original expiry.
        assert!(tracker.record_liveness(worker_id, &workflow_id, &id, start + window)?);
        assert!(tracker.is_live(worker_id, &workflow_id, &id, start + window + window)?);
        assert!(tracker.expired_workers(start + window + window)?.is_empty());

        // An untracked beat (an outbox dispatch, or a beat racing completion)
        // is a benign false, never an error.
        assert!(!tracker.record_liveness(
            worker_id,
            &workflow_id,
            &activity_id(52),
            start + window
        )?);
        Ok(())
    }

    #[test]
    fn sweep_interval_is_quarter_window_clamped_to_one_second_and_window() {
        // The default 30s window sweeps every 7.5s (quarter-window).
        assert_eq!(
            sweep_interval(Duration::from_secs(30)),
            Duration::from_millis(7_500)
        );
        // A short window's quarter (500ms) is floored at 1s.
        assert_eq!(
            sweep_interval(Duration::from_secs(2)),
            Duration::from_secs(1)
        );
        // A very long window's quarter stays within the [1s, window] band.
        assert_eq!(
            sweep_interval(Duration::from_secs(3_600)),
            Duration::from_secs(900)
        );
        // A sub-second (test) window sweeps once per window, never spinning
        // sub-window nor waiting longer than the window itself.
        assert_eq!(
            sweep_interval(Duration::from_millis(200)),
            Duration::from_millis(200)
        );
        // A zero window is floored at the minimum positive period rather than
        // producing the zero interval `tokio::time::interval` rejects.
        assert_eq!(sweep_interval(Duration::ZERO), Duration::from_millis(1));
    }
}
