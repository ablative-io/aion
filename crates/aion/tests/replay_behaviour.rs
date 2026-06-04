//! Behavioural replay tests over the in-memory event store.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use aion::durability::{
    Command, CorrelationKey, DurabilityError, LiveActivityOutcome, LiveChildOutcome, LiveExecutor,
    Recorder, Replay, ReplayOutcome, ReplayStep, ReplayTerminal, Resolution,
};
use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, Payload, RunId, TimerId, WorkflowId,
};
use aion_store::{EventStore, InMemoryStore};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

fn workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x1111))
}

fn child_workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x2222))
}

fn run_id() -> RunId {
    RunId::new(Uuid::from_u128(0x3333))
}

fn timestamp(seconds: i64) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| format!("invalid timestamp {seconds}").into())
}

fn payload(label: &str) -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "label": label }))?)
}

fn activity_command(ordinal: u64) -> Result<Command, Box<dyn std::error::Error>> {
    Ok(Command::RunActivity {
        key: CorrelationKey::Activity(ordinal),
        activity_type: "activity".to_owned(),
        input: payload("activity-input")?,
    })
}

fn timer_command(timer_id: TimerId) -> Result<Command, Box<dyn std::error::Error>> {
    Ok(Command::StartTimer {
        key: CorrelationKey::Timer(timer_id),
        fire_at: timestamp(100)?,
    })
}

fn signal_command(name: &str, index: usize) -> Command {
    Command::AwaitSignal {
        key: CorrelationKey::Signal {
            name: name.to_owned(),
            index,
        },
    }
}

fn child_command(child_start_seq: u64) -> Result<Command, Box<dyn std::error::Error>> {
    Ok(Command::SpawnChild {
        key: CorrelationKey::Child(child_start_seq),
        workflow_type: "child".to_owned(),
        input: payload("child-input")?,
    })
}

async fn record_full_history(
    store: Arc<dyn EventStore>,
) -> Result<Vec<aion_core::Event>, Box<dyn std::error::Error>> {
    let mut recorder = Recorder::new(workflow_id(), Arc::clone(&store));
    let activity_id = ActivityId::from_sequence_position(0);
    let timer_id = TimerId::anonymous(4);

    recorder
        .record_workflow_started(timestamp(10)?, "workflow".to_owned(), payload("input")?)
        .await?;
    recorder
        .record_activity_scheduled(
            timestamp(20)?,
            activity_id.clone(),
            "activity".to_owned(),
            payload("activity-input")?,
        )
        .await?;
    recorder
        .record_activity_completed(timestamp(30)?, activity_id, payload("activity-result")?)
        .await?;
    recorder
        .record_timer_started(timestamp(40)?, timer_id.clone(), timestamp(100)?)
        .await?;
    recorder
        .record_timer_fired(timestamp(50)?, timer_id)
        .await?;
    recorder
        .record_signal_received(
            timestamp(60)?,
            "ready".to_owned(),
            payload("signal-payload")?,
        )
        .await?;
    recorder
        .record_child_workflow_started(
            timestamp(70)?,
            child_workflow_id(),
            "child".to_owned(),
            payload("child-input")?,
        )
        .await?;
    recorder
        .record_child_workflow_completed(
            timestamp(80)?,
            child_workflow_id(),
            payload("child-result")?,
        )
        .await?;
    recorder
        .record_workflow_completed(timestamp(90)?, payload("workflow-result")?)
        .await?;

    Ok(store.read_history(&workflow_id()).await?)
}

async fn record_partial_history(
    store: Arc<dyn EventStore>,
) -> Result<Vec<aion_core::Event>, Box<dyn std::error::Error>> {
    let mut recorder = Recorder::new(workflow_id(), Arc::clone(&store));
    let activity_id = ActivityId::from_sequence_position(0);

    recorder
        .record_workflow_started(timestamp(10)?, "workflow".to_owned(), payload("input")?)
        .await?;
    recorder
        .record_activity_scheduled(
            timestamp(20)?,
            activity_id.clone(),
            "activity".to_owned(),
            payload("activity-input")?,
        )
        .await?;
    recorder
        .record_activity_completed(timestamp(30)?, activity_id, payload("activity-result")?)
        .await?;

    Ok(store.read_history(&workflow_id()).await?)
}

#[derive(Default)]
struct CountingExecutor {
    calls: AtomicUsize,
}

impl CountingExecutor {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn count_call(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl LiveExecutor for CountingExecutor {
    async fn run_activity(
        &self,
        activity_type: String,
        input: Payload,
    ) -> Result<LiveActivityOutcome, DurabilityError> {
        self.count_call();
        drop((activity_type, input));
        Err(DurabilityError::HistoryShape {
            reason: "replay must not invoke live activity execution".to_owned(),
        })
    }

    async fn start_timer(
        &self,
        timer_id: TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), DurabilityError> {
        self.count_call();
        drop((timer_id, fire_at));
        Err(DurabilityError::HistoryShape {
            reason: "replay must not invoke live timer execution".to_owned(),
        })
    }

    async fn await_signal(&self, name: String, index: usize) -> Result<Payload, DurabilityError> {
        self.count_call();
        drop((name, index));
        Err(DurabilityError::HistoryShape {
            reason: "replay must not invoke live signal execution".to_owned(),
        })
    }

    async fn spawn_child(
        &self,
        workflow_type: String,
        input: Payload,
    ) -> Result<LiveChildOutcome, DurabilityError> {
        self.count_call();
        drop((workflow_type, input));
        Err(DurabilityError::HistoryShape {
            reason: "replay must not invoke live child execution".to_owned(),
        })
    }
}

#[tokio::test]
async fn fully_recorded_history_replays_to_terminal_with_zero_live_calls()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let history = record_full_history(store).await?;
    let workflow_id = workflow_id();
    let run_id = run_id();
    let mut replay = Replay::new(&workflow_id, &run_id, history)?;
    let executor = CountingExecutor::default();

    let commands = vec![
        activity_command(0)?,
        timer_command(TimerId::anonymous(4))?,
        signal_command("ready", 0),
        child_command(7)?,
    ];
    let outcome = replay.drive(commands)?;

    assert_eq!(
        outcome,
        ReplayOutcome::Terminal {
            terminal: ReplayTerminal::Completed(payload("workflow-result")?),
            recorded: vec![
                Resolution::ActivityCompleted(payload("activity-result")?),
                Resolution::TimerFired,
                Resolution::SignalDelivered(payload("signal-payload")?),
                Resolution::ChildCompleted(payload("child-result")?),
            ],
        }
    );
    assert_eq!(executor.calls(), 0);
    Ok(())
}

#[tokio::test]
async fn partial_history_reports_resume_point_and_last_recorded_timestamp()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let history = record_partial_history(store).await?;
    let workflow_id = workflow_id();
    let run_id = run_id();
    let mut replay = Replay::new(&workflow_id, &run_id, history)?;
    let resume_command = timer_command(TimerId::anonymous(4))?;

    let outcome = replay.drive(vec![activity_command(0)?, resume_command.clone()])?;

    assert_eq!(
        outcome,
        ReplayOutcome::ResumeLive {
            command_index: 1,
            command: resume_command,
            recorded: vec![Resolution::ActivityCompleted(payload("activity-result")?)],
        }
    );
    assert_eq!(replay.now(), timestamp(30)?);
    Ok(())
}

#[tokio::test]
async fn activity_completed_is_served_from_history_cache_without_live_call()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let history = record_partial_history(store).await?;
    let workflow_id = workflow_id();
    let run_id = run_id();
    let mut replay = Replay::new(&workflow_id, &run_id, history)?;
    let executor = CountingExecutor::default();

    let step = replay.step(&activity_command(0)?)?;

    assert_eq!(
        step,
        ReplayStep::Recorded(Resolution::ActivityCompleted(payload("activity-result")?))
    );
    assert_eq!(executor.calls(), 0);
    Ok(())
}

#[tokio::test]
async fn terminal_activity_failure_is_served_from_history_cache_without_live_call()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let mut recorder = Recorder::new(workflow_id(), Arc::clone(&store));
    let activity_id = ActivityId::from_sequence_position(0);
    let terminal_error = ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: "terminal activity failure".to_owned(),
        details: None,
    };
    let executor = CountingExecutor::default();

    recorder
        .record_workflow_started(timestamp(10)?, "workflow".to_owned(), payload("input")?)
        .await?;
    recorder
        .record_activity_scheduled(
            timestamp(20)?,
            activity_id.clone(),
            "activity".to_owned(),
            payload("activity-input")?,
        )
        .await?;
    recorder
        .record_activity_failed(timestamp(30)?, activity_id, terminal_error.clone(), 1)
        .await?;

    let history = store.read_history(&workflow_id()).await?;
    let workflow_id = workflow_id();
    let run_id = run_id();
    let mut replay = Replay::new(&workflow_id, &run_id, history)?;

    assert_eq!(
        replay.step(&activity_command(0)?)?,
        ReplayStep::Recorded(Resolution::ActivityFailedTerminal(terminal_error))
    );
    assert_eq!(executor.calls(), 0);
    Ok(())
}
