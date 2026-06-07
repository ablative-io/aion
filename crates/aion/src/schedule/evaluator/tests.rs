use std::sync::{Arc, Mutex};
use std::time::Duration;

use aion_core::{
    CatchUpPolicy, Event, EventEnvelope, OverlapPolicy, Payload, ScheduleConfig, ScheduleId,
    TimerId, TriggerSpec, WorkflowId,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;

use super::{
    ScheduleEvaluator, ScheduleEvaluatorError, ScheduleEventSink, ScheduleEventSource,
    ScheduleTimer, ScheduleWorkflowCanceller, ScheduleWorkflowStarter, TimerEvaluationOutcome,
};
use crate::schedule::{ScheduleExecution, ScheduleState};

#[derive(Default)]
struct RecordingTimer {
    armed: Mutex<Vec<(ScheduleId, TimerId, DateTime<Utc>)>>,
}

#[async_trait]
impl ScheduleTimer for RecordingTimer {
    async fn arm_schedule_timer(
        &self,
        schedule_id: &ScheduleId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError> {
        self.armed
            .lock()
            .map_err(|error| {
                ScheduleEvaluatorError::side_effect(format!("timer lock poisoned: {error}"))
            })?
            .push((schedule_id.clone(), timer_id.clone(), fire_at));
        Ok(())
    }
}

#[derive(Default)]
struct RecordingStarter {
    started: Mutex<Vec<(String, Payload)>>,
}

#[async_trait]
impl ScheduleWorkflowStarter for RecordingStarter {
    async fn start_scheduled_workflow(
        &self,
        workflow_type: &str,
        input: Payload,
    ) -> Result<ScheduleExecution, ScheduleEvaluatorError> {
        self.started
            .lock()
            .map_err(|error| {
                ScheduleEvaluatorError::side_effect(format!("starter lock poisoned: {error}"))
            })?
            .push((workflow_type.to_owned(), input));
        Ok(ScheduleExecution::new(
            WorkflowId::new_v4(),
            aion_core::RunId::new_v4(),
        ))
    }
}

#[derive(Default)]
struct RecordingCanceller {
    cancelled: Mutex<Vec<ScheduleExecution>>,
}

#[async_trait]
impl ScheduleWorkflowCanceller for RecordingCanceller {
    async fn cancel_scheduled_workflow(
        &self,
        execution: &ScheduleExecution,
        reason: &str,
    ) -> Result<(), ScheduleEvaluatorError> {
        let _ = reason;
        self.cancelled
            .lock()
            .map_err(|error| {
                ScheduleEvaluatorError::side_effect(format!("canceller lock poisoned: {error}"))
            })?
            .push(execution.clone());
        Ok(())
    }
}

#[derive(Default)]
struct RecordingEvents {
    triggered: Mutex<Vec<(ScheduleId, ScheduleExecution, DateTime<Utc>)>>,
}

#[async_trait]
impl ScheduleEventSink for RecordingEvents {
    async fn record_schedule_triggered(
        &self,
        schedule_id: &ScheduleId,
        execution: &ScheduleExecution,
        recorded_at: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError> {
        self.triggered
            .lock()
            .map_err(|error| {
                ScheduleEvaluatorError::side_effect(format!("events lock poisoned: {error}"))
            })?
            .push((schedule_id.clone(), execution.clone(), recorded_at));
        Ok(())
    }
}

struct VecEventSource {
    events: Vec<Event>,
}

#[async_trait]
impl ScheduleEventSource for VecEventSource {
    async fn schedule_events(&self) -> Result<Vec<Event>, ScheduleEvaluatorError> {
        Ok(self.events.clone())
    }
}

struct Fixture {
    evaluator: ScheduleEvaluator,
    timer: Arc<RecordingTimer>,
    starter: Arc<RecordingStarter>,
    canceller: Arc<RecordingCanceller>,
    events: Arc<RecordingEvents>,
}

fn fixture() -> Fixture {
    let timer = Arc::new(RecordingTimer::default());
    let starter = Arc::new(RecordingStarter::default());
    let canceller = Arc::new(RecordingCanceller::default());
    let events = Arc::new(RecordingEvents::default());
    let evaluator = ScheduleEvaluator::new(
        timer.clone(),
        starter.clone(),
        canceller.clone(),
        events.clone(),
    );
    Fixture {
        evaluator,
        timer,
        starter,
        canceller,
        events,
    }
}

fn parse_utc(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value).map(|date_time| date_time.with_timezone(&Utc))
}

fn config(
    overlap_policy: OverlapPolicy,
    catch_up_policy: CatchUpPolicy,
) -> Result<ScheduleConfig, aion_core::PayloadError> {
    Ok(ScheduleConfig {
        trigger: TriggerSpec::Interval {
            period: Duration::from_secs(60),
        },
        overlap_policy,
        catch_up_policy,
        workflow_type: String::from("checkout"),
        input: Payload::from_json(&json!({ "source": "schedule" }))?,
    })
}

fn state(
    overlap_policy: OverlapPolicy,
    catch_up_policy: CatchUpPolicy,
    next_trigger_at: DateTime<Utc>,
) -> Result<ScheduleState, Box<dyn std::error::Error>> {
    let schedule_id = ScheduleId::new_v4();
    let created_at = next_trigger_at - chrono::Duration::seconds(60);
    let mut state = ScheduleState::created(
        schedule_id,
        config(overlap_policy, catch_up_policy)?,
        created_at,
    )?;
    state.set_next_trigger_at(next_trigger_at);
    Ok(state)
}

fn envelope(seq: u64, recorded_at: DateTime<Utc>) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at,
        workflow_id: WorkflowId::new_v4(),
    }
}

fn lock_len<T>(mutex: &Mutex<Vec<T>>) -> Result<usize, ScheduleEvaluatorError> {
    Ok(mutex
        .lock()
        .map_err(|error| {
            ScheduleEvaluatorError::side_effect(format!("test lock poisoned: {error}"))
        })?
        .len())
}

#[tokio::test]
async fn timer_fire_starts_records_and_rearms_next_fire() -> Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = fixture();
    let fire_at = parse_utc("2026-06-07T00:01:00Z")?;
    let schedule_state = state(OverlapPolicy::AllowAll, CatchUpPolicy::One, fire_at)?;
    let schedule_id = schedule_state.schedule_id.clone();
    fixture.evaluator.upsert_state(schedule_state);

    let outcome = fixture
        .evaluator
        .handle_timer_fired(&schedule_id, fire_at)
        .await?;

    assert!(matches!(outcome, TimerEvaluationOutcome::Started(_)));
    assert_eq!(lock_len(&fixture.starter.started)?, 1);
    assert_eq!(lock_len(&fixture.events.triggered)?, 1);
    let armed = fixture
        .timer
        .armed
        .lock()
        .map_err(|error| format!("timer lock poisoned: {error}"))?;
    assert_eq!(armed.len(), 1);
    assert_eq!(armed[0].0, schedule_id);
    assert_eq!(armed[0].2, parse_utc("2026-06-07T00:02:00Z")?);
    Ok(())
}

#[tokio::test]
async fn paused_and_deleted_schedules_do_not_arm() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let next = parse_utc("2026-06-07T00:01:00Z")?;
    let mut paused = state(OverlapPolicy::Skip, CatchUpPolicy::One, next)?;
    let paused_id = paused.schedule_id.clone();
    paused.is_paused = true;
    let mut deleted = state(OverlapPolicy::Skip, CatchUpPolicy::One, next)?;
    let deleted_id = deleted.schedule_id.clone();
    deleted.is_deleted = true;
    fixture.evaluator.upsert_state(paused);
    fixture.evaluator.upsert_state(deleted);

    assert!(!fixture.evaluator.arm_active_schedule(&paused_id).await?);
    assert!(!fixture.evaluator.arm_active_schedule(&deleted_id).await?);
    assert_eq!(lock_len(&fixture.timer.armed)?, 0);
    Ok(())
}

#[tokio::test]
async fn skip_overlap_skips_and_rearms() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let fire_at = parse_utc("2026-06-07T00:01:00Z")?;
    let mut schedule_state = state(OverlapPolicy::Skip, CatchUpPolicy::One, fire_at)?;
    schedule_state.current_execution = Some(ScheduleExecution::new(
        WorkflowId::new_v4(),
        aion_core::RunId::new_v4(),
    ));
    let schedule_id = schedule_state.schedule_id.clone();
    fixture.evaluator.upsert_state(schedule_state);

    let outcome = fixture
        .evaluator
        .handle_timer_fired(&schedule_id, fire_at)
        .await?;

    assert_eq!(outcome, TimerEvaluationOutcome::Skipped);
    assert_eq!(lock_len(&fixture.starter.started)?, 0);
    assert_eq!(lock_len(&fixture.timer.armed)?, 1);
    Ok(())
}

#[tokio::test]
async fn buffer_one_overlap_queues_at_most_one() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let fire_at = parse_utc("2026-06-07T00:01:00Z")?;
    let mut schedule_state = state(OverlapPolicy::BufferOne, CatchUpPolicy::One, fire_at)?;
    schedule_state.current_execution = Some(ScheduleExecution::new(
        WorkflowId::new_v4(),
        aion_core::RunId::new_v4(),
    ));
    let schedule_id = schedule_state.schedule_id.clone();
    fixture.evaluator.upsert_state(schedule_state);

    let first = fixture
        .evaluator
        .handle_timer_fired(&schedule_id, fire_at)
        .await?;
    let second = fixture
        .evaluator
        .handle_timer_fired(&schedule_id, fire_at + chrono::Duration::seconds(60))
        .await?;

    assert_eq!(first, TimerEvaluationOutcome::Buffered);
    assert_eq!(second, TimerEvaluationOutcome::Skipped);
    assert!(fixture.evaluator.has_pending_tick(&schedule_id));
    assert_eq!(lock_len(&fixture.starter.started)?, 0);
    Ok(())
}

#[tokio::test]
async fn cancel_previous_overlap_cancels_then_starts() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let fire_at = parse_utc("2026-06-07T00:01:00Z")?;
    let active = ScheduleExecution::new(WorkflowId::new_v4(), aion_core::RunId::new_v4());
    let mut schedule_state = state(OverlapPolicy::CancelPrevious, CatchUpPolicy::One, fire_at)?;
    schedule_state.current_execution = Some(active);
    let schedule_id = schedule_state.schedule_id.clone();
    fixture.evaluator.upsert_state(schedule_state);

    let outcome = fixture
        .evaluator
        .handle_timer_fired(&schedule_id, fire_at)
        .await?;

    assert!(matches!(outcome, TimerEvaluationOutcome::Started(_)));
    assert_eq!(lock_len(&fixture.canceller.cancelled)?, 1);
    assert_eq!(lock_len(&fixture.starter.started)?, 1);
    Ok(())
}

#[tokio::test]
async fn allow_all_overlap_always_starts() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let fire_at = parse_utc("2026-06-07T00:01:00Z")?;
    let mut schedule_state = state(OverlapPolicy::AllowAll, CatchUpPolicy::One, fire_at)?;
    schedule_state.current_execution = Some(ScheduleExecution::new(
        WorkflowId::new_v4(),
        aion_core::RunId::new_v4(),
    ));
    let schedule_id = schedule_state.schedule_id.clone();
    fixture.evaluator.upsert_state(schedule_state);

    let outcome = fixture
        .evaluator
        .handle_timer_fired(&schedule_id, fire_at)
        .await?;

    assert!(matches!(outcome, TimerEvaluationOutcome::Started(_)));
    assert_eq!(lock_len(&fixture.starter.started)?, 1);
    Ok(())
}

#[tokio::test]
async fn recovery_applies_all_catch_up_and_records_triggers()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let first = parse_utc("2026-06-07T00:01:00Z")?;
    let now = parse_utc("2026-06-07T00:03:00Z")?;
    let schedule_state = state(OverlapPolicy::AllowAll, CatchUpPolicy::All, first)?;
    fixture.evaluator.upsert_state(schedule_state);

    fixture.evaluator.recover_projected_state(now).await?;

    assert_eq!(lock_len(&fixture.events.triggered)?, 3);
    assert_eq!(lock_len(&fixture.timer.armed)?, 1);
    Ok(())
}

#[tokio::test]
async fn recovery_applies_one_catch_up_and_records_trigger()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let first = parse_utc("2026-06-07T00:01:00Z")?;
    let now = parse_utc("2026-06-07T00:03:00Z")?;
    let schedule_state = state(OverlapPolicy::AllowAll, CatchUpPolicy::One, first)?;
    fixture.evaluator.upsert_state(schedule_state);

    fixture.evaluator.recover_projected_state(now).await?;

    assert_eq!(lock_len(&fixture.events.triggered)?, 1);
    assert_eq!(lock_len(&fixture.timer.armed)?, 1);
    Ok(())
}

#[tokio::test]
async fn recovery_applies_skip_catch_up_without_recording_trigger()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let first = parse_utc("2026-06-07T00:01:00Z")?;
    let now = parse_utc("2026-06-07T00:03:00Z")?;
    let schedule_state = state(OverlapPolicy::AllowAll, CatchUpPolicy::Skip, first)?;
    fixture.evaluator.upsert_state(schedule_state);

    fixture.evaluator.recover_projected_state(now).await?;

    assert_eq!(lock_len(&fixture.events.triggered)?, 0);
    assert_eq!(lock_len(&fixture.timer.armed)?, 1);
    Ok(())
}

#[tokio::test]
async fn recovery_reconstructs_state_and_rearms_only_active_schedules()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = fixture();
    let created_at = parse_utc("2026-06-07T00:00:00Z")?;
    let now = parse_utc("2026-06-07T00:00:30Z")?;
    let active_id = ScheduleId::new_v4();
    let paused_id = ScheduleId::new_v4();
    let deleted_id = ScheduleId::new_v4();
    let schedule_config = config(OverlapPolicy::AllowAll, CatchUpPolicy::One)?;
    let events = vec![
        Event::ScheduleCreated {
            envelope: envelope(1, created_at),
            schedule_id: active_id.clone(),
            config: schedule_config.clone(),
        },
        Event::ScheduleCreated {
            envelope: envelope(2, created_at),
            schedule_id: paused_id.clone(),
            config: schedule_config.clone(),
        },
        Event::SchedulePaused {
            envelope: envelope(3, created_at),
            schedule_id: paused_id,
        },
        Event::ScheduleCreated {
            envelope: envelope(4, created_at),
            schedule_id: deleted_id.clone(),
            config: schedule_config,
        },
        Event::ScheduleDeleted {
            envelope: envelope(5, created_at),
            schedule_id: deleted_id,
        },
    ];
    let source = VecEventSource { events };

    fixture.evaluator.recover_on_startup(&source, now).await?;

    let armed = fixture
        .timer
        .armed
        .lock()
        .map_err(|error| format!("timer lock poisoned: {error}"))?;
    assert_eq!(armed.len(), 1);
    assert_eq!(armed[0].0, active_id);
    Ok(())
}
