//! Durability coverage for the `HaematiteStore` observability (`O`) keyspace.
//!
//! These tests exercise the real haematite backend behind the
//! [`aion_store::ObservabilityStore`] contract: append/read/head round-trips,
//! the `SequenceConflict` optimistic-concurrency signal the server's sequencer
//! retries on, within-attempt dedup, and — the load-bearing durability guarantee
//! — that an `O`-region record survives a database reopen AND is structurally
//! undecodable as a workflow-history `Event`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_core::{ActivityEvent, ActivityEventKind, ActivityId, Event, MessageRole, WorkflowId};
use aion_store::{ActivityStreamKey, ObservabilityStore, ReadableEventStore, StoreError};
use aion_store_haematite::HaematiteStore;
use chrono::Utc;
use uuid::Uuid;

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aion-store-haematite-{name}-{}-{nanos}-{counter}",
        std::process::id()
    ))
}

fn workflow() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0xABCD))
}

fn event(attempt: u32, worker_seq: u64, text: &str) -> ActivityEvent {
    ActivityEvent {
        workflow_id: workflow(),
        activity_id: ActivityId::from_sequence_position(3),
        attempt,
        agent_id: Uuid::from_u128(9),
        agent_role: "orchestrator".to_owned(),
        emitted_at: Utc::now(),
        worker_seq,
        store_seq: None,
        ephemeral: false,
        kind: ActivityEventKind::Message {
            role: MessageRole::Assistant,
            text: text.to_owned(),
        },
    }
}

fn key(attempt: u32) -> ActivityStreamKey {
    ActivityStreamKey::new(workflow(), ActivityId::from_sequence_position(3), attempt)
}

#[tokio::test(flavor = "multi_thread")]
async fn append_read_head_round_trip() -> Result<(), StoreError> {
    let store = HaematiteStore::create(unique_temp_dir("obs-round-trip"))?;
    assert_eq!(store.activity_head(&key(0)).await?, 0);
    assert_eq!(store.append_activity_event(0, &event(0, 1, "a")).await?, 0);
    assert_eq!(store.append_activity_event(1, &event(0, 2, "b")).await?, 1);
    assert_eq!(store.append_activity_event(2, &event(0, 3, "c")).await?, 2);
    assert_eq!(store.activity_head(&key(0)).await?, 3);

    let all = store.read_activity_events_from(&key(0), 0).await?;
    assert_eq!(all.len(), 3);
    for (expected, record) in (0u64..).zip(all.iter()) {
        assert_eq!(record.store_seq, expected);
        assert_eq!(record.event.store_seq, Some(expected));
    }
    // Resume by store_seq.
    let tail = store.read_activity_events_from(&key(0), 2).await?;
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].store_seq, 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_expected_seq_conflicts_and_writes_nothing() -> Result<(), StoreError> {
    let store = HaematiteStore::create(unique_temp_dir("obs-conflict"))?;
    store.append_activity_event(0, &event(0, 1, "a")).await?;
    // Re-appending at the already-consumed seq 0 conflicts against head 1 — the
    // exact signal the server sequencer re-reads-head-and-retries on.
    let conflict = store.append_activity_event(0, &event(0, 2, "dup")).await;
    assert_eq!(
        conflict,
        Err(StoreError::SequenceConflict {
            expected: 0,
            found: 1
        })
    );
    // Nothing partial written.
    assert_eq!(store.read_activity_events_from(&key(0), 0).await?.len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn attempts_are_disjoint_streams() -> Result<(), StoreError> {
    let store = HaematiteStore::create(unique_temp_dir("obs-attempts"))?;
    store
        .append_activity_event(0, &event(0, 1, "attempt-0"))
        .await?;
    store
        .append_activity_event(0, &event(1, 1, "attempt-1"))
        .await?;
    assert_eq!(store.activity_head(&key(0)).await?, 1);
    assert_eq!(store.activity_head(&key(1)).await?, 1);
    let a0 = store.read_activity_events_from(&key(0), 0).await?;
    let a1 = store.read_activity_events_from(&key(1), 0).await?;
    assert_eq!(a0.len(), 1);
    assert_eq!(a1.len(), 1);
    // Same store_seq space, different attempt streams.
    assert_eq!(a0[0].store_seq, 0);
    assert_eq!(a1[0].store_seq, 0);
    Ok(())
}

/// THE durability guarantee (§7.5 / §0.6): an `O`-region record survives a
/// database reopen AND the workflow-history replay path (`read_history`) cannot
/// see it — the `O` key is byte-disjoint from the `E`-stream, so replay never
/// scans it and could not decode it as an `Event`. This is what makes
/// "durable but non-replay-authoritative" a structural fact.
#[tokio::test(flavor = "multi_thread")]
async fn observability_records_survive_reopen_and_are_invisible_to_replay() -> Result<(), StoreError>
{
    let dir = unique_temp_dir("obs-durable");
    {
        let store = HaematiteStore::create(&dir)?;
        store
            .append_activity_event(0, &event(0, 1, "durable-a"))
            .await?;
        store
            .append_activity_event(1, &event(0, 2, "durable-b"))
            .await?;
        store
            .event_store()
            .database()
            .commit()
            .map_err(|error| StoreError::Backend(format!("commit failed: {error}")))?;
    }
    // Reopen the SAME on-disk database.
    let reopened = HaematiteStore::open(&dir)?;

    // The observability transcript survived kill-and-reopen.
    let records = reopened.read_activity_events_from(&key(0), 0).await?;
    assert_eq!(
        records.len(),
        2,
        "observability records must survive reopen"
    );
    assert_eq!(records[0].store_seq, 0);
    assert_eq!(records[1].store_seq, 1);

    // The workflow-history replay path cannot see them: `read_history` decodes
    // ONLY the `E`-stream, and the `O` key is not an `E`-stream key. The workflow
    // has zero replay-authoritative history despite two durable `O` records.
    let history: Vec<Event> = reopened.read_history(&workflow()).await?;
    assert!(
        history.is_empty(),
        "observability records must never appear in workflow replay history: {history:?}"
    );
    Ok(())
}
