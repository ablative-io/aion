//! Persistence coverage for `LibSQL` local-file stores across close and reopen.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_store::{
    ContentType, Event, EventEnvelope, EventStore, Payload, StoreError, TimerEntry, TimerId,
    WorkflowId,
};
use aion_store_libsql::LibSqlStore;
use chrono::{DateTime, Utc};
use serde_json::json;

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn history_active_list_and_timers_survive_reopen() -> Result<(), StoreError> {
    let path = unique_temp_path("durable-state");
    let running = workflow_id(1);
    let completed = workflow_id(2);
    let due_timer = TimerEntry {
        workflow_id: running.clone(),
        timer_id: TimerId::anonymous(10),
        fire_at: recorded_at(50),
    };
    let future_timer = TimerEntry {
        workflow_id: running.clone(),
        timer_id: TimerId::anonymous(11),
        fire_at: recorded_at(500),
    };
    let running_history = vec![
        workflow_started(1, &running, "checkout"),
        signal_received(2, &running, "wake"),
    ];
    let completed_history = vec![
        workflow_started(1, &completed, "billing"),
        workflow_completed(2, &completed),
    ];

    {
        let store = LibSqlStore::open(path.clone()).await?;
        store.append(&running, &running_history, 0).await?;
        store.append(&completed, &completed_history, 0).await?;
        store
            .schedule_timer(
                &due_timer.workflow_id,
                &due_timer.timer_id,
                due_timer.fire_at,
            )
            .await?;
        store
            .schedule_timer(
                &future_timer.workflow_id,
                &future_timer.timer_id,
                future_timer.fire_at,
            )
            .await?;
    }

    let reopened = LibSqlStore::open(path).await?;

    assert_eq!(reopened.read_history(&running).await?, running_history);
    assert_eq!(reopened.read_history(&completed).await?, completed_history);
    assert_eq!(reopened.list_active().await?, vec![running.clone()]);
    assert_eq!(
        reopened.expired_timers(recorded_at(100)).await?,
        vec![due_timer]
    );

    Ok(())
}

#[tokio::test]
async fn sequence_guard_uses_persisted_head_after_reopen() -> Result<(), StoreError> {
    let path = unique_temp_path("sequence-head");
    let workflow = workflow_id(3);
    let first_batch = vec![
        workflow_started(1, &workflow, "checkout"),
        signal_received(2, &workflow, "first"),
    ];
    let second_batch = vec![signal_received(3, &workflow, "second")];

    {
        let store = LibSqlStore::open(path.clone()).await?;
        store.append(&workflow, &first_batch, 0).await?;
    }

    let reopened = LibSqlStore::open(path).await?;
    reopened.append(&workflow, &second_batch, 2).await?;

    let stale_batch = vec![signal_received(4, &workflow, "stale")];
    assert_eq!(
        reopened.append(&workflow, &stale_batch, 0).await,
        Err(StoreError::SequenceConflict {
            expected: 0,
            found: 3,
        })
    );

    let mut expected_history = first_batch;
    expected_history.extend(second_batch);
    assert_eq!(reopened.read_history(&workflow).await?, expected_history);

    Ok(())
}

fn workflow_id(value: u128) -> WorkflowId {
    WorkflowId::new(uuid::Uuid::from_u128(value))
}

fn workflow_started(seq: u64, workflow_id: &WorkflowId, workflow_type: &str) -> Event {
    Event::WorkflowStarted {
        envelope: envelope(seq, workflow_id),
        workflow_type: workflow_type.to_owned(),
        input: payload("input"),
    }
}

fn workflow_completed(seq: u64, workflow_id: &WorkflowId) -> Event {
    Event::WorkflowCompleted {
        envelope: envelope(seq, workflow_id),
        result: payload("result"),
    }
}

fn signal_received(seq: u64, workflow_id: &WorkflowId, name: &str) -> Event {
    Event::SignalReceived {
        envelope: envelope(seq, workflow_id),
        name: name.to_owned(),
        payload: payload("signal"),
    }
}

fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: recorded_at(seq_as_offset(seq)),
        workflow_id: workflow_id.clone(),
    }
}

fn payload(label: &str) -> Payload {
    match Payload::from_json(&json!({ "label": label })) {
        Ok(payload) => payload,
        Err(error) => Payload::new(
            ContentType::Json,
            format!("{{\"payload_error\":\"{error}\"}}").into_bytes(),
        ),
    }
}

fn recorded_at(offset_seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn seq_as_offset(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

fn unique_temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!(
        "aion-store-libsql-{name}-{}-{nanos}-{counter}.db",
        std::process::id()
    ))
}
