//! Tests for the receive loop and bounded concurrency.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload, RunId, WorkflowId,
};
use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
use async_trait::async_trait;
use futures::stream;
use serde_json::json;
use tokio::sync::{Mutex, mpsc};

use super::{ActivityDispatcher, DispatchOutcome, ServeEnd, serve_activity_tasks};
use crate::context::ActivityContext;
use crate::error::WorkerError;
use crate::protocol::{
    ActivityTask, PendingActivityReport, UnackedResultTracker, WorkerSession, WorkerSessionEvent,
    WorkerTaskStream, validate_activity_handlers,
};
use crate::{ReconnectConfig, WorkerConfig};

#[derive(Default)]
struct FakeSession {
    tasks: Vec<Result<WorkerSessionEvent, WorkerError>>,
    reports: Vec<RecordedReport>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordedReport {
    Completed(ActivityId, Payload),
    Failed(ActivityId, ActivityError),
}

#[async_trait]
impl WorkerSession for FakeSession {
    async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
        drop(config.clone());
        Ok(())
    }

    async fn register(
        &mut self,
        activity_types: Vec<String>,
        available_handlers: &BTreeSet<String>,
    ) -> Result<(), WorkerError> {
        validate_activity_handlers(&activity_types, available_handlers)
    }

    fn receive_tasks(&mut self) -> WorkerTaskStream {
        Box::pin(stream::iter(std::mem::take(&mut self.tasks)))
    }

    async fn report_result(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        result: Payload,
    ) -> Result<(), WorkerError> {
        let _ = (workflow_id, run_id);
        self.reports
            .push(RecordedReport::Completed(activity_id, result));
        Ok(())
    }

    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        let _ = (workflow_id, run_id);
        self.reports
            .push(RecordedReport::Failed(activity_id, failure));
        Ok(())
    }

    async fn send_heartbeat(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        progress: Option<Payload>,
    ) -> Result<(), WorkerError> {
        drop((workflow_id, activity_id, progress));
        Ok(())
    }
}

struct RecordingDispatcher {
    outcomes: Mutex<Vec<DispatchOutcome>>,
    dispatched: Mutex<Vec<ActivityId>>,
}

#[async_trait]
impl ActivityDispatcher for RecordingDispatcher {
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError> {
        self.dispatched.lock().await.push(task.activity_id.clone());
        drop(context);
        let mut outcomes = self.outcomes.lock().await;
        if outcomes.is_empty() {
            return Err(WorkerError::decode(NoOutcome));
        }
        Ok(outcomes.remove(0))
    }

    fn activity_types(&self) -> BTreeSet<String> {
        [String::from("charge-card")].into_iter().collect()
    }
}

struct SlowDispatcher {
    current: AtomicUsize,
    peak: AtomicUsize,
    started: AtomicUsize,
    release: AtomicBool,
}

#[async_trait]
impl ActivityDispatcher for SlowDispatcher {
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        update_peak(&self.peak, now);
        self.started.fetch_add(1, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        self.current.fetch_sub(1, Ordering::SeqCst);
        drop((task, context));
        Ok(DispatchOutcome::Completed {
            output: Payload::new(ContentType::Json, b"{}".to_vec()),
        })
    }

    fn activity_types(&self) -> BTreeSet<String> {
        [String::from("slow")].into_iter().collect()
    }
}

struct CancellingDispatcher {
    started: tokio::sync::Notify,
    observed_cancelled: AtomicBool,
}

#[async_trait]
impl ActivityDispatcher for CancellingDispatcher {
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError> {
        drop(task);
        self.started.notify_waiters();
        context.cancelled().await;
        self.observed_cancelled
            .store(context.is_cancelled(), Ordering::SeqCst);
        Ok(DispatchOutcome::Completed {
            output: Payload::new(ContentType::Json, b"{}".to_vec()),
        })
    }

    fn activity_types(&self) -> BTreeSet<String> {
        [String::from("cancellable")].into_iter().collect()
    }
}

#[tokio::test]
async fn dispatches_two_tasks_and_reports_corresponding_outcomes() -> Result<(), WorkerError> {
    let workflow_id = WorkflowId::new_v4();
    let first_activity = ActivityId::from_sequence_position(1);
    let second_activity = ActivityId::from_sequence_position(2);
    let first_output = Payload::from_json(&json!({"ok": true})).map_err(WorkerError::encode)?;
    let failure = ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: String::from("invalid card"),
        details: None,
    };
    let mut session = FakeSession {
        tasks: vec![
            Ok(WorkerSessionEvent::Task(proto_task(
                workflow_id.clone(),
                first_activity.clone(),
                "charge-card",
            ))),
            Ok(WorkerSessionEvent::Task(proto_task(
                workflow_id.clone(),
                second_activity.clone(),
                "charge-card",
            ))),
        ],
        reports: Vec::new(),
    };
    let dispatcher = Arc::new(RecordingDispatcher {
        outcomes: Mutex::new(vec![
            DispatchOutcome::Completed {
                output: first_output.clone(),
            },
            DispatchOutcome::Failed {
                failure: failure.clone(),
            },
        ]),
        dispatched: Mutex::new(Vec::new()),
    });
    let config = test_config(2);
    let mut tracker = UnackedResultTracker::new();

    let end =
        serve_activity_tasks(&config, &mut session, Arc::clone(&dispatcher), &mut tracker).await?;

    assert_eq!(end, ServeEnd::StreamClosed);
    assert_eq!(
        *dispatcher.dispatched.lock().await,
        vec![first_activity.clone(), second_activity.clone()]
    );
    assert_eq!(
        session.reports,
        vec![
            RecordedReport::Completed(first_activity.clone(), first_output),
            RecordedReport::Failed(second_activity.clone(), failure),
        ]
    );
    assert_eq!(tracker.len(), 2);
    assert!(matches!(
        tracker.get(&workflow_id, &first_activity),
        Some(PendingActivityReport::Completed { .. })
    ));
    assert!(matches!(
        tracker.get(&workflow_id, &second_activity),
        Some(PendingActivityReport::Failed { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn max_concurrency_caps_dispatches_at_two() -> Result<(), WorkerError> {
    let workflow_id = WorkflowId::new_v4();
    let (task_sender, task_receiver) = mpsc::channel(5);
    let mut session = ChannelSession {
        receiver: Some(task_receiver),
        reports: Vec::new(),
    };
    for position in 1..=5 {
        task_sender
            .send(Ok(WorkerSessionEvent::Task(proto_task(
                workflow_id.clone(),
                ActivityId::from_sequence_position(position),
                "slow",
            ))))
            .await
            .map_err(WorkerError::decode)?;
    }
    drop(task_sender);
    let dispatcher = Arc::new(SlowDispatcher {
        current: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        started: AtomicUsize::new(0),
        release: AtomicBool::new(false),
    });
    let config = test_config(2);
    let worker = tokio::spawn({
        let dispatcher = Arc::clone(&dispatcher);
        async move {
            let mut tracker = UnackedResultTracker::new();
            let result =
                serve_activity_tasks(&config, &mut session, dispatcher, &mut tracker).await;
            (result, session, tracker)
        }
    });

    wait_until_started(&dispatcher.started, 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(dispatcher.started.load(Ordering::SeqCst), 2);
    assert_eq!(dispatcher.peak.load(Ordering::SeqCst), 2);

    dispatcher.release.store(true, Ordering::SeqCst);
    let (result, session, tracker) = worker.await.map_err(WorkerError::decode)?;
    assert_eq!(result?, ServeEnd::StreamClosed);

    assert_eq!(session.reports.len(), 5);
    assert_eq!(tracker.len(), 5);
    assert_eq!(dispatcher.peak.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn cancellation_event_flips_context_without_suppressing_result() -> Result<(), WorkerError> {
    let workflow_id = WorkflowId::new_v4();
    let activity_id = ActivityId::from_sequence_position(9);
    let (event_sender, event_receiver) = mpsc::channel(2);
    let mut session = ChannelSession {
        receiver: Some(event_receiver),
        reports: Vec::new(),
    };
    event_sender
        .send(Ok(WorkerSessionEvent::Task(proto_task(
            workflow_id.clone(),
            activity_id.clone(),
            "cancellable",
        ))))
        .await
        .map_err(WorkerError::decode)?;
    let dispatcher = Arc::new(CancellingDispatcher {
        started: tokio::sync::Notify::new(),
        observed_cancelled: AtomicBool::new(false),
    });
    let config = test_config(1);
    let worker = tokio::spawn({
        let dispatcher = Arc::clone(&dispatcher);
        async move {
            let mut tracker = UnackedResultTracker::new();
            let result =
                serve_activity_tasks(&config, &mut session, dispatcher, &mut tracker).await;
            (result, session)
        }
    });

    dispatcher.started.notified().await;
    event_sender
        .send(Ok(WorkerSessionEvent::Cancel {
            workflow_id,
            activity_id: activity_id.clone(),
        }))
        .await
        .map_err(WorkerError::decode)?;
    drop(event_sender);
    let (result, session) = worker.await.map_err(WorkerError::decode)?;
    assert_eq!(result?, ServeEnd::StreamClosed);

    assert!(dispatcher.observed_cancelled.load(Ordering::SeqCst));
    assert_eq!(session.reports.len(), 1);
    assert!(matches!(
        &session.reports[0],
        RecordedReport::Completed(reported_id, _) if reported_id == &activity_id
    ));
    Ok(())
}

struct ChannelSession {
    receiver: Option<mpsc::Receiver<Result<WorkerSessionEvent, WorkerError>>>,
    reports: Vec<RecordedReport>,
}

#[async_trait]
impl WorkerSession for ChannelSession {
    async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
        drop(config.clone());
        Ok(())
    }

    async fn register(
        &mut self,
        activity_types: Vec<String>,
        available_handlers: &BTreeSet<String>,
    ) -> Result<(), WorkerError> {
        validate_activity_handlers(&activity_types, available_handlers)
    }

    fn receive_tasks(&mut self) -> WorkerTaskStream {
        match self.receiver.take() {
            Some(receiver) => Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
            None => Box::pin(stream::empty()),
        }
    }

    async fn report_result(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        result: Payload,
    ) -> Result<(), WorkerError> {
        let _ = (workflow_id, run_id);
        self.reports
            .push(RecordedReport::Completed(activity_id, result));
        Ok(())
    }

    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        let _ = (workflow_id, run_id);
        self.reports
            .push(RecordedReport::Failed(activity_id, failure));
        Ok(())
    }

    async fn send_heartbeat(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        progress: Option<Payload>,
    ) -> Result<(), WorkerError> {
        drop((workflow_id, activity_id, progress));
        Ok(())
    }
}

fn proto_task(
    workflow_id: WorkflowId,
    activity_id: ActivityId,
    activity_type: &str,
) -> ProtoActivityTask {
    ProtoActivityTask {
        workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
        activity_id: Some(ProtoActivityId::from(activity_id)),
        run_id: None,
        activity_type: String::from(activity_type),
        input: Some(ProtoPayload::from(Payload::new(
            ContentType::Json,
            b"{}".to_vec(),
        ))),
        attempt: 1,
        labels: std::collections::HashMap::new(),
    }
}

async fn wait_until_started(started: &AtomicUsize, expected: usize) {
    while started.load(Ordering::SeqCst) < expected {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn update_peak(peak: &AtomicUsize, observed: usize) {
    let mut current_peak = peak.load(Ordering::SeqCst);
    while observed > current_peak {
        match peak.compare_exchange(current_peak, observed, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return,
            Err(next_peak) => current_peak = next_peak,
        }
    }
}

fn test_config(max_concurrency: usize) -> WorkerConfig {
    WorkerConfig::new(
        "http://127.0.0.1:50051",
        "payments",
        "worker-a",
        max_concurrency,
        ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 3),
        None,
    )
}

#[derive(Debug, thiserror::Error)]
#[error("fake dispatcher has no canned outcome")]
struct NoOutcome;

/// Dispatcher that records the attempt each context exposes.
struct AttemptRecordingDispatcher {
    attempts: Mutex<Vec<u32>>,
}

#[async_trait]
impl ActivityDispatcher for AttemptRecordingDispatcher {
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError> {
        drop(task);
        self.attempts.lock().await.push(context.attempt());
        Ok(DispatchOutcome::Completed {
            output: Payload::new(ContentType::Json, b"{}".to_vec()),
        })
    }

    fn activity_types(&self) -> BTreeSet<String> {
        [String::from("charge-card")].into_iter().collect()
    }
}

/// Brief test 22 (first half): the wire attempt is surfaced verbatim on the
/// handler's `ActivityContext`.
#[tokio::test]
async fn wire_attempt_is_exposed_on_the_activity_context() -> Result<(), WorkerError> {
    let workflow_id = WorkflowId::new_v4();
    let mut task = proto_task(
        workflow_id,
        ActivityId::from_sequence_position(1),
        "charge-card",
    );
    task.attempt = 3;
    let mut session = FakeSession {
        tasks: vec![Ok(WorkerSessionEvent::Task(task))],
        reports: Vec::new(),
    };
    let dispatcher = Arc::new(AttemptRecordingDispatcher {
        attempts: Mutex::new(Vec::new()),
    });
    let mut tracker = UnackedResultTracker::new();

    let end = serve_activity_tasks(
        &test_config(1),
        &mut session,
        Arc::clone(&dispatcher),
        &mut tracker,
    )
    .await?;

    assert_eq!(end, ServeEnd::StreamClosed);
    assert_eq!(*dispatcher.attempts.lock().await, vec![3]);
    Ok(())
}

/// Brief test 22 (second half): a wire task whose attempt is zero is a
/// malformed task — the serve loop surfaces a decode error (a budgeted
/// retryable drop for the run loop), never a defaulted attempt.
#[tokio::test]
async fn zero_attempt_task_fails_serve_with_decode_error() -> Result<(), WorkerError> {
    let workflow_id = WorkflowId::new_v4();
    let mut task = proto_task(
        workflow_id,
        ActivityId::from_sequence_position(1),
        "charge-card",
    );
    task.attempt = 0;
    let mut session = FakeSession {
        tasks: vec![Ok(WorkerSessionEvent::Task(task))],
        reports: Vec::new(),
    };
    let dispatcher = Arc::new(AttemptRecordingDispatcher {
        attempts: Mutex::new(Vec::new()),
    });
    let mut tracker = UnackedResultTracker::new();

    let result = serve_activity_tasks(
        &test_config(1),
        &mut session,
        Arc::clone(&dispatcher),
        &mut tracker,
    )
    .await;

    assert!(matches!(result, Err(WorkerError::Decode { .. })));
    assert!(dispatcher.attempts.lock().await.is_empty());
    Ok(())
}

/// A drain frame ends the serve loop with the dedicated `Drained` end and
/// latches `SessionHealth::drain_received` for the run loop's classifier.
#[tokio::test]
async fn drain_frame_ends_serve_as_drained_and_latches_health() -> Result<(), WorkerError> {
    let mut session = FakeSession {
        tasks: vec![Ok(WorkerSessionEvent::Drain)],
        reports: Vec::new(),
    };
    let dispatcher = Arc::new(AttemptRecordingDispatcher {
        attempts: Mutex::new(Vec::new()),
    });
    let mut tracker = UnackedResultTracker::new();
    let mut health = super::SessionHealth::default();

    let end = super::serve_activity_tasks_until(
        &test_config(1),
        &mut session,
        Arc::clone(&dispatcher),
        &mut tracker,
        &mut health,
        futures::future::pending(),
    )
    .await?;

    assert_eq!(end, ServeEnd::Drained);
    assert!(health.drain_received, "the drain latch must be set");
    Ok(())
}

/// One recorded heartbeat frame: task identity plus the optional progress.
type HeartbeatRecord = (WorkflowId, ActivityId, Option<Payload>);
/// Shared heartbeat log a test observes while the serve loop owns the session.
type HeartbeatLog = Arc<std::sync::Mutex<Vec<HeartbeatRecord>>>;

/// Snapshot the shared heartbeat log (lint-clean: a poisoned log is a test error).
fn heartbeat_snapshot(log: &HeartbeatLog) -> Result<Vec<HeartbeatRecord>, WorkerError> {
    log.lock()
        .map(|entries| entries.clone())
        .map_err(|_| WorkerError::Transport {
            source: tonic::Status::internal("heartbeat log mutex poisoned"),
        })
}

/// Channel-driven session that carries a server-assigned heartbeat window and
/// records every heartbeat frame through a shared handle, so a test can watch
/// the automatic liveness pump while the serve loop runs.
struct WindowSession {
    receiver: Option<mpsc::Receiver<Result<WorkerSessionEvent, WorkerError>>>,
    reports: Vec<RecordedReport>,
    heartbeat_window: Option<Duration>,
    heartbeats: HeartbeatLog,
}

#[async_trait]
impl WorkerSession for WindowSession {
    async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
        drop(config.clone());
        Ok(())
    }

    async fn register(
        &mut self,
        activity_types: Vec<String>,
        available_handlers: &BTreeSet<String>,
    ) -> Result<(), WorkerError> {
        validate_activity_handlers(&activity_types, available_handlers)
    }

    fn receive_tasks(&mut self) -> WorkerTaskStream {
        match self.receiver.take() {
            Some(receiver) => Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
            None => Box::pin(stream::empty()),
        }
    }

    async fn report_result(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        result: Payload,
    ) -> Result<(), WorkerError> {
        let _ = (workflow_id, run_id);
        self.reports
            .push(RecordedReport::Completed(activity_id, result));
        Ok(())
    }

    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        let _ = (workflow_id, run_id);
        self.reports
            .push(RecordedReport::Failed(activity_id, failure));
        Ok(())
    }

    async fn send_heartbeat(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        progress: Option<Payload>,
    ) -> Result<(), WorkerError> {
        let mut log = self.heartbeats.lock().map_err(|_| WorkerError::Transport {
            source: tonic::Status::internal("heartbeat log mutex poisoned"),
        })?;
        log.push((workflow_id, activity_id, progress));
        Ok(())
    }

    fn heartbeat_window(&self) -> Option<Duration> {
        self.heartbeat_window
    }
}

/// Dispatcher whose handler runs until released and NEVER calls
/// `ActivityContext::heartbeat` — the worst case for server-side liveness.
struct HeldDispatcher {
    release: Arc<AtomicBool>,
}

#[async_trait]
impl ActivityDispatcher for HeldDispatcher {
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError> {
        drop((task, context));
        while !self.release.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        Ok(DispatchOutcome::Completed {
            output: Payload::new(ContentType::Json, b"{}".to_vec()),
        })
    }

    fn activity_types(&self) -> BTreeSet<String> {
        [String::from("charge-card")].into_iter().collect()
    }
}

/// #176 critical-finding regression proof (worker side): the RUNTIME
/// automatically heartbeats an in-flight activity whose handler never
/// heartbeats, at a cadence well inside the server-assigned window — so a
/// legitimately long activity can no longer be expired by the server's
/// heartbeat sweeper. The pump also stops once nothing is in flight.
#[tokio::test]
async fn runtime_auto_heartbeats_long_activity_within_the_window() -> Result<(), WorkerError> {
    const WINDOW: Duration = Duration::from_millis(100);
    let workflow_id = WorkflowId::new_v4();
    let activity_id = ActivityId::from_sequence_position(9);
    let heartbeats = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = Arc::new(AtomicBool::new(false));
    let (event_tx, event_rx) = mpsc::channel(4);
    let session = WindowSession {
        receiver: Some(event_rx),
        reports: Vec::new(),
        heartbeat_window: Some(WINDOW),
        heartbeats: Arc::clone(&heartbeats),
    };
    let dispatcher = Arc::new(HeldDispatcher {
        release: Arc::clone(&release),
    });

    let serve = tokio::spawn({
        let dispatcher = Arc::clone(&dispatcher);
        async move {
            let mut session = session;
            let mut tracker = UnackedResultTracker::new();
            let end =
                serve_activity_tasks(&test_config(1), &mut session, dispatcher, &mut tracker).await;
            (session, end)
        }
    });
    event_tx
        .send(Ok(WorkerSessionEvent::Task(proto_task(
            workflow_id.clone(),
            activity_id.clone(),
            "charge-card",
        ))))
        .await
        .map_err(|_| WorkerError::Transport {
            source: tonic::Status::internal("serve loop hung up prematurely"),
        })?;

    // A handler that outlives several windows must be beaten automatically.
    // Waiting for three beats proves at least one full window elapsed with
    // sub-window beats (pump cadence is a quarter window).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let beats = heartbeats.lock().map(|log| log.len()).unwrap_or_default();
        if beats >= 3 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "runtime never auto-heartbeated the in-flight activity"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let log = heartbeat_snapshot(&heartbeats)?;
    assert!(
        log.iter().all(|(wf, act, progress)| {
            *wf == workflow_id && *act == activity_id && progress.is_none()
        }),
        "automatic liveness beats must target the in-flight task and carry no progress: {log:?}"
    );

    // Release the handler; once its outcome is reported the pump must stop.
    // Give the reporter a moment to consume the outcome, then require beat
    // stability across several would-be pump intervals.
    release.store(true, Ordering::SeqCst);
    tokio::time::sleep(WINDOW).await;
    let settled = heartbeat_snapshot(&heartbeats)?.len();
    tokio::time::sleep(WINDOW * 2).await;
    let after = heartbeat_snapshot(&heartbeats)?.len();
    assert_eq!(
        settled, after,
        "the liveness pump must stop once nothing is in flight"
    );

    drop(event_tx);
    let (session, end) = serve.await.map_err(|_| WorkerError::Transport {
        source: tonic::Status::internal("serve task panicked"),
    })?;
    assert_eq!(end?, ServeEnd::StreamClosed);
    assert_eq!(
        session.reports,
        vec![RecordedReport::Completed(
            activity_id,
            Payload::new(ContentType::Json, b"{}".to_vec())
        )],
        "the held activity must still complete and report normally"
    );
    Ok(())
}

/// A session without a server-assigned window (every fake, and an
/// unregistered session) never auto-pumps: behaviour is byte-identical to the
/// pre-pump loop.
#[tokio::test]
async fn no_window_means_no_automatic_heartbeats() -> Result<(), WorkerError> {
    let heartbeats = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = Arc::new(AtomicBool::new(false));
    let (event_tx, event_rx) = mpsc::channel(4);
    let session = WindowSession {
        receiver: Some(event_rx),
        reports: Vec::new(),
        heartbeat_window: None,
        heartbeats: Arc::clone(&heartbeats),
    };
    let dispatcher = Arc::new(HeldDispatcher {
        release: Arc::clone(&release),
    });

    let serve = tokio::spawn(async move {
        let mut session = session;
        let mut tracker = UnackedResultTracker::new();
        serve_activity_tasks(&test_config(1), &mut session, dispatcher, &mut tracker).await
    });
    event_tx
        .send(Ok(WorkerSessionEvent::Task(proto_task(
            WorkflowId::new_v4(),
            ActivityId::from_sequence_position(10),
            "charge-card",
        ))))
        .await
        .map_err(|_| WorkerError::Transport {
            source: tonic::Status::internal("serve loop hung up prematurely"),
        })?;

    // Long enough that a (wrongly) armed pump at any plausible cadence would
    // have beaten several times.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        heartbeat_snapshot(&heartbeats)?.is_empty(),
        "a windowless session must never receive automatic heartbeats"
    );

    release.store(true, Ordering::SeqCst);
    drop(event_tx);
    let end = serve.await.map_err(|_| WorkerError::Transport {
        source: tonic::Status::internal("serve task panicked"),
    })??;
    assert_eq!(end, ServeEnd::StreamClosed);
    Ok(())
}

/// The pump cadence is a quarter of the server-assigned window, floored at
/// one millisecond so a degenerate zero window cannot panic the interval.
#[test]
fn liveness_pump_interval_is_quarter_window_floored_at_one_millisecond() {
    assert_eq!(
        super::liveness_pump_interval(Duration::from_secs(30)),
        Duration::from_millis(7_500)
    );
    assert_eq!(
        super::liveness_pump_interval(Duration::from_millis(100)),
        Duration::from_millis(25)
    );
    assert_eq!(
        super::liveness_pump_interval(Duration::ZERO),
        Duration::from_millis(1)
    );
}
