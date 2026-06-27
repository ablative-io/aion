//! Heartbeat window tracking and lost-worker failure surfacing.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

use aion_core::{ActivityId, Payload, WorkflowId};
use aion_proto::{ProtoHeartbeat, WireError};

use crate::error::ServerError;
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

/// Tasks failed because a worker was declared lost.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LostWorkerReport {
    /// Lost worker removed from the connected-worker registry.
    pub worker_id: WorkerId,
    /// In-flight activities surfaced to the engine as retryable failures.
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
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if tracker state cannot be trusted.
    pub fn complete_task(
        &self,
        worker_id: WorkerId,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<(), ServerError> {
        let key = TaskKey::new(worker_id, workflow_id.clone(), activity_id.clone());
        let became_empty = {
            let mut state = self.state()?;
            state.tasks.remove(&key);
            state.tasks.is_empty()
        };
        if became_empty {
            self.empty.notify_waiters();
        }
        Ok(())
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
        liveness.last_progress = decoded.progress;
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
        registry.deregister(worker_id)?;
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
    }

    impl ActivityCompletionSink for RecordingSink {
        fn complete_activity(&self, completion: ActivityCompletion) -> Result<(), ServerError> {
            self.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .push(completion);
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
}
