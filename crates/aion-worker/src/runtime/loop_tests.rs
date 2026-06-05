//! Tests for the receive loop and bounded concurrency.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload, WorkflowId};
use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
use async_trait::async_trait;
use futures::stream;
use serde_json::json;
use tokio::sync::{Mutex, mpsc};

use super::{
    ActivityDispatcher, DispatchOutcome, serve_activity_tasks, serve_activity_tasks_with_reconnect,
};
use crate::error::WorkerError;
use crate::protocol::{ActivityTask, WorkerSession, WorkerTaskStream, validate_activity_handlers};
use crate::{ReconnectConfig, WorkerConfig};

#[derive(Default)]
struct FakeSession {
    tasks: Vec<Result<ProtoActivityTask, WorkerError>>,
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
        result: Payload,
    ) -> Result<(), WorkerError> {
        let _ = workflow_id;
        self.reports
            .push(RecordedReport::Completed(activity_id, result));
        Ok(())
    }

    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        let _ = workflow_id;
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
    async fn dispatch(&self, task: ActivityTask) -> Result<DispatchOutcome, WorkerError> {
        self.dispatched.lock().await.push(task.activity_id.clone());
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
    async fn dispatch(&self, task: ActivityTask) -> Result<DispatchOutcome, WorkerError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        update_peak(&self.peak, now);
        self.started.fetch_add(1, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        self.current.fetch_sub(1, Ordering::SeqCst);
        drop(task);
        Ok(DispatchOutcome::Completed {
            output: Payload::new(ContentType::Json, b"{}".to_vec()),
        })
    }

    fn activity_types(&self) -> BTreeSet<String> {
        [String::from("slow")].into_iter().collect()
    }
}

struct SingleOutcomeDispatcher {
    outcome: DispatchOutcome,
    events: Arc<StdMutex<Vec<LoopEvent>>>,
}

#[async_trait]
impl ActivityDispatcher for SingleOutcomeDispatcher {
    async fn dispatch(&self, task: ActivityTask) -> Result<DispatchOutcome, WorkerError> {
        push_loop_event(
            &self.events,
            LoopEvent::Dispatch(task.activity_id.sequence_position()),
        )?;
        Ok(self.outcome.clone())
    }

    fn activity_types(&self) -> BTreeSet<String> {
        [String::from("charge-card")].into_iter().collect()
    }
}

struct ReconnectOrderingSession {
    name: &'static str,
    tasks: Vec<Result<ProtoActivityTask, WorkerError>>,
    events: Arc<StdMutex<Vec<LoopEvent>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LoopEvent {
    Handshake(&'static str),
    Register(&'static str),
    ReceiveTasks(&'static str),
    Dispatch(u64),
    ReportResult(&'static str, u64),
}

#[derive(Debug, thiserror::Error)]
#[error("loop event log mutex poisoned")]
struct PoisonedEventLog;

#[async_trait]
impl WorkerSession for ReconnectOrderingSession {
    async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
        let _ = config;
        push_loop_event(&self.events, LoopEvent::Handshake(self.name))
    }

    async fn register(
        &mut self,
        activity_types: Vec<String>,
        available_handlers: &BTreeSet<String>,
    ) -> Result<(), WorkerError> {
        validate_activity_handlers(&activity_types, available_handlers)?;
        push_loop_event(&self.events, LoopEvent::Register(self.name))
    }

    fn receive_tasks(&mut self) -> WorkerTaskStream {
        if push_loop_event(&self.events, LoopEvent::ReceiveTasks(self.name)).is_err() {
            return Box::pin(stream::iter([Err(WorkerError::decode(PoisonedEventLog))]));
        }
        Box::pin(stream::iter(std::mem::take(&mut self.tasks)))
    }

    async fn report_result(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        result: Payload,
    ) -> Result<(), WorkerError> {
        drop((workflow_id, result));
        push_loop_event(
            &self.events,
            LoopEvent::ReportResult(self.name, activity_id.sequence_position()),
        )
    }

    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        drop((workflow_id, activity_id, failure));
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
            Ok(proto_task(
                workflow_id.clone(),
                first_activity.clone(),
                "charge-card",
            )),
            Ok(proto_task(
                workflow_id.clone(),
                second_activity.clone(),
                "charge-card",
            )),
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
    let config = WorkerConfig::new(
        "http://127.0.0.1:50051",
        "payments",
        "worker-a",
        2,
        ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 3),
        None,
    );

    serve_activity_tasks(&config, &mut session, Arc::clone(&dispatcher)).await?;

    assert_eq!(
        *dispatcher.dispatched.lock().await,
        vec![first_activity.clone(), second_activity.clone()]
    );
    assert_eq!(
        session.reports,
        vec![
            RecordedReport::Completed(first_activity, first_output),
            RecordedReport::Failed(second_activity, failure),
        ]
    );
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
            .send(Ok(proto_task(
                workflow_id.clone(),
                ActivityId::from_sequence_position(position),
                "slow",
            )))
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
    let config = WorkerConfig::new(
        "http://127.0.0.1:50051",
        "payments",
        "worker-a",
        2,
        ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 3),
        None,
    );
    let worker = tokio::spawn({
        let dispatcher = Arc::clone(&dispatcher);
        async move {
            let result = serve_activity_tasks(&config, &mut session, dispatcher).await;
            (result, session)
        }
    });

    wait_until_started(&dispatcher.started, 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(dispatcher.started.load(Ordering::SeqCst), 2);
    assert_eq!(dispatcher.peak.load(Ordering::SeqCst), 2);

    dispatcher.release.store(true, Ordering::SeqCst);
    let (result, session) = worker.await.map_err(WorkerError::decode)?;
    result?;

    assert_eq!(session.reports.len(), 5);
    assert_eq!(dispatcher.peak.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn reconnect_re_reports_backlog_before_receiving_new_tasks() -> Result<(), WorkerError> {
    let workflow_id = WorkflowId::new_v4();
    let first_activity = ActivityId::from_sequence_position(1);
    let second_activity = ActivityId::from_sequence_position(2);
    let events = Arc::new(StdMutex::new(Vec::new()));
    let first_session = ReconnectOrderingSession {
        name: "first",
        tasks: vec![
            Ok(proto_task(
                workflow_id.clone(),
                first_activity,
                "charge-card",
            )),
            Err(WorkerError::Transport {
                source: tonic::Status::unavailable("disconnect before ack"),
            }),
        ],
        events: Arc::clone(&events),
    };
    let reconnect_events = Arc::clone(&events);
    let reconnect_workflow = workflow_id.clone();
    let reconnect_activity = second_activity;
    let dispatcher = Arc::new(SingleOutcomeDispatcher {
        outcome: DispatchOutcome::Completed {
            output: Payload::new(ContentType::Json, b"{}".to_vec()),
        },
        events: Arc::clone(&events),
    });
    let config = WorkerConfig::new(
        "http://127.0.0.1:50051",
        "payments",
        "worker-a",
        2,
        ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 2),
        None,
    );

    serve_activity_tasks_with_reconnect(&config, first_session, dispatcher, move || {
        let reconnect_events = Arc::clone(&reconnect_events);
        let reconnect_workflow = reconnect_workflow.clone();
        let reconnect_activity = reconnect_activity.clone();
        async move {
            Ok(ReconnectOrderingSession {
                name: "second",
                tasks: vec![Ok(proto_task(
                    reconnect_workflow,
                    reconnect_activity,
                    "charge-card",
                ))],
                events: reconnect_events,
            })
        }
    })
    .await?;

    assert_eq!(
        loop_events(&events)?,
        vec![
            LoopEvent::ReceiveTasks("first"),
            LoopEvent::Dispatch(1),
            LoopEvent::ReportResult("first", 1),
            LoopEvent::Handshake("second"),
            LoopEvent::Register("second"),
            LoopEvent::ReportResult("second", 1),
            LoopEvent::ReceiveTasks("second"),
            LoopEvent::Dispatch(2),
            LoopEvent::ReportResult("second", 2),
        ]
    );
    Ok(())
}

struct ChannelSession {
    receiver: Option<mpsc::Receiver<Result<ProtoActivityTask, WorkerError>>>,
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
        result: Payload,
    ) -> Result<(), WorkerError> {
        let _ = workflow_id;
        self.reports
            .push(RecordedReport::Completed(activity_id, result));
        Ok(())
    }

    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        let _ = workflow_id;
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
        activity_type: String::from(activity_type),
        input: Some(ProtoPayload::from(Payload::new(
            ContentType::Json,
            b"{}".to_vec(),
        ))),
    }
}

fn push_loop_event(
    events: &Arc<StdMutex<Vec<LoopEvent>>>,
    event: LoopEvent,
) -> Result<(), WorkerError> {
    events
        .lock()
        .map_err(|_| WorkerError::decode(PoisonedEventLog))?
        .push(event);
    Ok(())
}

fn loop_events(events: &Arc<StdMutex<Vec<LoopEvent>>>) -> Result<Vec<LoopEvent>, WorkerError> {
    Ok(events
        .lock()
        .map_err(|_| WorkerError::decode(PoisonedEventLog))?
        .clone())
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

#[derive(Debug, thiserror::Error)]
#[error("fake dispatcher has no canned outcome")]
struct NoOutcome;
