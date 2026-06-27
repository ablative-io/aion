//! Dedicated non-determinism violation integration tests.

use std::sync::Arc;

use aion::durability::{
    Command, CorrelationKey, DurabilityError, HistoryCursor, NON_DETERMINISM_WORKFLOW_ERROR_PREFIX,
    Recorder, Resolution, ResolveOutcome, Resolver, fail_on_violation,
};
use aion_core::{ActivityId, Event, Payload, TimerId, WorkflowId};
use aion_store::{EventStore, InMemoryStore};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

fn workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::nil())
}

fn timestamp(seconds: i64) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| "invalid timestamp".into())
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

async fn store_with_activity_history() -> Result<Arc<dyn EventStore>, Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let mut recorder = Recorder::new(workflow_id(), Arc::clone(&store));
    let activity_id = ActivityId::from_sequence_position(0);
    recorder
        .record_activity_scheduled(
            timestamp(1)?,
            activity_id.clone(),
            "activity".to_owned(),
            payload("activity-input")?,
            String::from("default"),
            None,
        )
        .await?;
    recorder
        .record_activity_completed(timestamp(2)?, activity_id, payload("activity-result")?)
        .await?;
    Ok(store)
}

async fn resolver_from_store(
    store: &Arc<dyn EventStore>,
) -> Result<Resolver, Box<dyn std::error::Error>> {
    let history = store.read_history(&workflow_id()).await?;
    let cursor = HistoryCursor::new(history)?;
    Ok(Resolver::new(workflow_id(), cursor))
}

#[tokio::test]
async fn wrong_family_reports_typed_non_determinism_error() -> Result<(), Box<dyn std::error::Error>>
{
    let store = store_with_activity_history().await?;
    let mut resolver = resolver_from_store(&store).await?;

    let error = resolver
        .resolve(Command::StartTimer {
            key: CorrelationKey::Timer(TimerId::anonymous(7)),
            fire_at: timestamp(10)?,
        })
        .err()
        .ok_or_else(|| "expected non-determinism error".to_owned())?;

    let DurabilityError::NonDeterminism(error) = error else {
        return Err("expected typed non-determinism error".to_owned().into());
    };
    assert_eq!(error.workflow_id, workflow_id());
    assert_eq!(error.seq, 1);
    assert_eq!(error.expected, "Timer Timer(TimerId(Anonymous(7)))");
    assert!(error.found.contains("ActivityScheduled"));
    assert!(error.found.contains("family Some(Activity)"));
    assert!(error.found.contains("key Some(Activity(0))"));

    Ok(())
}

#[tokio::test]
async fn wrong_correlation_key_reports_typed_non_determinism_error()
-> Result<(), Box<dyn std::error::Error>> {
    let store = store_with_activity_history().await?;
    let mut resolver = resolver_from_store(&store).await?;

    let error = resolver
        .resolve(activity_command(1)?)
        .err()
        .ok_or_else(|| "expected non-determinism error".to_owned())?;

    let DurabilityError::NonDeterminism(error) = error else {
        return Err("expected typed non-determinism error".to_owned().into());
    };
    assert_eq!(error.workflow_id, workflow_id());
    assert_eq!(error.seq, 1);
    assert_eq!(error.expected, "Activity Activity(1)");
    assert!(error.found.contains("ActivityScheduled"));
    assert!(error.found.contains("family Some(Activity)"));
    assert!(error.found.contains("key Some(Activity(0))"));

    Ok(())
}

#[tokio::test]
async fn matching_command_stream_does_not_false_positive() -> Result<(), Box<dyn std::error::Error>>
{
    let store = store_with_activity_history().await?;
    let mut resolver = resolver_from_store(&store).await?;

    let outcome = resolver.resolve(activity_command(0)?)?;

    assert_eq!(
        outcome,
        ResolveOutcome::Recorded(Resolution::ActivityCompleted(payload("activity-result")?))
    );

    Ok(())
}

#[tokio::test]
async fn violation_failure_records_one_non_determinism_workflow_failed()
-> Result<(), Box<dyn std::error::Error>> {
    let store = store_with_activity_history().await?;
    let mut resolver = resolver_from_store(&store).await?;
    let error = resolver
        .resolve(activity_command(1)?)
        .err()
        .ok_or_else(|| "expected non-determinism error".to_owned())?;
    let DurabilityError::NonDeterminism(violation) = error else {
        return Err("expected typed non-determinism error".to_owned().into());
    };

    let mut recorder = Recorder::resume_at(workflow_id(), Arc::clone(&store), 2);
    fail_on_violation(&mut recorder, timestamp(3)?, &violation).await?;

    let history = store.read_history(&workflow_id()).await?;
    let failures: Vec<_> = history
        .iter()
        .filter_map(|event| match event {
            Event::WorkflowFailed { envelope, error } => Some((envelope.seq, error)),
            _ => None,
        })
        .collect();

    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, 3);
    assert!(
        failures[0]
            .1
            .message
            .starts_with(NON_DETERMINISM_WORKFLOW_ERROR_PREFIX)
    );
    assert!(
        failures[0]
            .1
            .message
            .contains("expected Activity Activity(1)")
    );
    assert!(failures[0].1.message.contains("found ActivityScheduled"));

    Ok(())
}
