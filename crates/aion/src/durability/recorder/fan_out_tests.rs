//! Unit tests for [`Recorder::record_fan_out_dispatch`]: the atomic
//! `N×(ActivityScheduled+ActivityStarted)` events + `N` outbox rows write.
//!
//! These tests exercise the Recorder against an outbox-aware test store double
//! ([`OutboxTestStore`]) that mirrors the libSQL atomicity contract: events and outbox rows for one
//! `append_with_outbox` call either both commit or neither does. The double can be armed to fail the
//! atomic append so the test can assert the all-or-nothing guarantee without a real database.

use std::sync::{Arc, Mutex};

use aion_core::{Event, Payload, RunId, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_store::package::{PackageRecord, PackageRouteRecord, PackageStore};
use aion_store::visibility::{
    ListWorkflowsFilter, VisibilityRecord, VisibilityStore,
    WorkflowSummary as VisibilityWorkflowSummary,
};
use aion_store::{
    InMemoryStore, OutboxRow, OutboxStatus, ReadableEventStore, RunSummary, StoreError, TimerEntry,
    WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;

use super::{FanOutCompletionResult, FanOutItem, FanOutOutcome, Recorder};
use crate::durability::DurabilityError;

/// Outbox-aware test store: delegates reads/timers/packages/event-only appends to an inner
/// [`InMemoryStore`], records outbox rows in a `Mutex<Vec<OutboxRow>>`, and applies the atomic
/// `append_with_outbox` all-or-nothing. When `fail_outbox_append` is set, the atomic append writes
/// nothing (no events, no outbox rows) and returns a backend error, modelling a transaction abort.
#[derive(Debug)]
struct OutboxTestStore {
    inner: InMemoryStore,
    outbox: Mutex<Vec<OutboxRow>>,
    fail_outbox_append: bool,
}

impl OutboxTestStore {
    fn new() -> Self {
        Self {
            inner: InMemoryStore::default(),
            outbox: Mutex::new(Vec::new()),
            fail_outbox_append: false,
        }
    }

    fn failing() -> Self {
        Self {
            inner: InMemoryStore::default(),
            outbox: Mutex::new(Vec::new()),
            fail_outbox_append: true,
        }
    }

    fn outbox_rows(&self) -> Result<Vec<OutboxRow>, StoreError> {
        Ok(self
            .outbox
            .lock()
            .map_err(|error| StoreError::Backend(format!("outbox lock poisoned: {error}")))?
            .clone())
    }
}

#[async_trait]
impl WritableEventStore for OutboxTestStore {
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        self.inner
            .append(token, workflow_id, events, expected_seq)
            .await
    }

    async fn append_with_outbox(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[OutboxRow],
    ) -> Result<(), StoreError> {
        if self.fail_outbox_append {
            // Model a transaction abort: write neither events nor outbox rows.
            return Err(StoreError::Backend(String::from(
                "forced outbox append failure",
            )));
        }
        // Events first under the sequence guard; if that conflicts, the outbox rows are never
        // written, so the two stay atomic (no partial commit).
        self.inner
            .append(token, workflow_id, events, expected_seq)
            .await?;
        self.outbox
            .lock()
            .map_err(|error| StoreError::Backend(format!("outbox lock poisoned: {error}")))?
            .extend_from_slice(outbox_rows);
        Ok(())
    }
}

#[async_trait]
impl ReadableEventStore for OutboxTestStore {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history(workflow_id).await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history_from(workflow_id, from_seq).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.inner.read_run_chain(workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_workflow_ids().await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_active().await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        self.inner.query(filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner
            .schedule_timer(workflow_id, timer_id, fire_at)
            .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        self.inner.expired_timers(as_of).await
    }
}

#[async_trait]
impl PackageStore for OutboxTestStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        self.inner.put_package(record).await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        self.inner.list_packages().await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner.delete_package(workflow_type, content_hash).await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner
            .put_package_route(workflow_type, content_hash)
            .await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        self.inner.list_package_routes().await
    }
}

#[async_trait]
impl VisibilityStore for OutboxTestStore {
    async fn record_visibility(&self, record: VisibilityRecord) -> Result<(), StoreError> {
        self.inner.record_visibility(record).await
    }

    async fn list_workflows(
        &self,
        filter: ListWorkflowsFilter,
    ) -> Result<Vec<VisibilityWorkflowSummary>, StoreError> {
        self.inner.list_workflows(filter).await
    }

    async fn count_workflows(&self, filter: ListWorkflowsFilter) -> Result<u64, StoreError> {
        self.inner.count_workflows(filter).await
    }
}

fn workflow_id(value: u128) -> WorkflowId {
    WorkflowId::new(uuid::Uuid::from_u128(value))
}

fn recorded_at(offset_seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
}

fn payload(label: &str) -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "label": label }))?)
}

fn fan_out_items(
    base_ordinal: u64,
    count: u64,
) -> Result<Vec<FanOutItem>, Box<dyn std::error::Error>> {
    let mut items = Vec::new();
    for offset in 0..count {
        let ordinal = base_ordinal + offset;
        items.push(FanOutItem {
            ordinal,
            namespace: "test-ns".to_owned(),
            task_queue: "test-tq".to_owned(),
            node: None,
            activity_type: format!("activity-{ordinal}"),
            input: payload(&format!("input-{ordinal}"))?,
            attempt: 1,
        });
    }
    Ok(items)
}

/// (a) Exactly `2N` events, contiguous seqs, `Scheduled` then `Started` per ordinal in order;
/// (d) the head advanced by exactly `2N`.
#[tokio::test]
async fn fan_out_dispatch_appends_2n_events_in_scheduled_started_order()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(1);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());
    let items = fan_out_items(0, 3)?;

    recorder
        .record_fan_out_dispatch(recorded_at(1), &items)
        .await?;

    let history = store.read_history(&workflow_id).await?;
    assert_eq!(history.len(), 6, "N=3 must append exactly 2N=6 events");
    for (index, event) in history.iter().enumerate() {
        let expected_seq = u64::try_from(index)? + 1;
        assert_eq!(event.seq(), expected_seq, "seqs must be contiguous from 1");
    }
    for (member, chunk) in history.chunks(2).enumerate() {
        let ordinal = u64::try_from(member)?;
        let expected_activity_id = aion_core::ActivityId::from_sequence_position(ordinal);
        match &chunk[0] {
            Event::ActivityScheduled {
                activity_id,
                activity_type,
                ..
            } => {
                assert_eq!(activity_id, &expected_activity_id);
                assert_eq!(activity_type, &format!("activity-{ordinal}"));
            }
            other => return Err(format!("expected ActivityScheduled, got {other:?}").into()),
        }
        match &chunk[1] {
            Event::ActivityStarted { activity_id, .. } => {
                assert_eq!(activity_id, &expected_activity_id);
            }
            other => return Err(format!("expected ActivityStarted, got {other:?}").into()),
        }
    }
    assert_eq!(recorder.current_head(), 6, "head advances by exactly 2N");
    Ok(())
}

/// (b) The `N` outbox rows are present with `dispatch_key = "{workflow_id}:{ordinal}"`, pending,
/// attempt zero, and the per-item activity type / input.
#[tokio::test]
async fn fan_out_dispatch_stages_n_outbox_rows_with_dispatch_keys()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(2);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());
    let items = fan_out_items(5, 4)?;

    recorder
        .record_fan_out_dispatch(recorded_at(1), &items)
        .await?;

    let rows = store.outbox_rows()?;
    assert_eq!(rows.len(), 4, "N=4 must stage exactly N=4 outbox rows");
    for (offset, row) in rows.iter().enumerate() {
        let ordinal = 5 + u64::try_from(offset)?;
        assert_eq!(row.ordinal, ordinal);
        assert_eq!(
            row.dispatch_key,
            format!("{workflow_id}:{ordinal}"),
            "dispatch_key must be {{workflow_id}}:{{ordinal}}"
        );
        assert_eq!(row.workflow_id, workflow_id);
        assert_eq!(row.activity_type, format!("activity-{ordinal}"));
        assert_eq!(row.input, payload(&format!("input-{ordinal}"))?);
        assert_eq!(row.status, OutboxStatus::Pending);
        assert_eq!(row.attempt, 0);
        assert_eq!(row.visible_after, recorded_at(1));
        // NSTQ-2: the staged row carries the workflow's routing identity off the item.
        assert_eq!(row.namespace, "test-ns");
        assert_eq!(row.task_queue, "test-tq");
    }
    Ok(())
}

/// (c) One atomic op: a forced store failure leaves the head unadvanced AND no events AND no
/// outbox rows.
#[tokio::test]
async fn fan_out_dispatch_is_atomic_on_store_failure() -> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(3);
    let store = Arc::new(OutboxTestStore::failing());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());
    let items = fan_out_items(0, 3)?;

    let result = recorder
        .record_fan_out_dispatch(recorded_at(1), &items)
        .await;

    assert!(
        result.is_err(),
        "forced store failure must surface as an error"
    );
    assert!(matches!(
        result,
        Err(DurabilityError::Store(StoreError::Backend(_)))
    ));
    assert_eq!(
        recorder.current_head(),
        0,
        "head must not advance on failure"
    );
    assert!(
        store.read_history(&workflow_id).await?.is_empty(),
        "no events may be written on failure"
    );
    assert!(
        store.outbox_rows()?.is_empty(),
        "no outbox rows may be written on failure"
    );
    Ok(())
}

/// A sequence conflict surfaces as a hard error WITHOUT advancing the head and without writing
/// events or outbox rows (mirrors `sequence_conflict_surfaces_without_advancing_or_retrying`).
#[tokio::test]
async fn fan_out_dispatch_sequence_conflict_surfaces_without_advancing()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(4);
    let store = Arc::new(OutboxTestStore::new());
    // A rogue writer advances the real head to 1 behind the recorder's back.
    let rogue = Event::WorkflowStarted {
        envelope: aion_core::EventEnvelope {
            seq: 1,
            recorded_at: recorded_at(1),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: String::from("checkout"),
        input: payload("rogue")?,
        run_id: RunId::new(uuid::Uuid::from_u128(1)),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    };
    store
        .append(WriteToken::recorder(), &workflow_id, &[rogue], 0)
        .await?;

    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());
    let items = fan_out_items(0, 2)?;
    let result = recorder
        .record_fan_out_dispatch(recorded_at(2), &items)
        .await;

    match result {
        Err(DurabilityError::Store(StoreError::SequenceConflict { expected, found })) => {
            assert_eq!(expected, 0);
            assert_eq!(found, 1);
        }
        Err(other) => return Err(format!("expected sequence conflict, got {other:?}").into()),
        Ok(()) => return Err("expected sequence conflict".into()),
    }
    assert_eq!(
        recorder.current_head(),
        0,
        "conflict must not advance the head"
    );
    assert_eq!(
        store.read_history(&workflow_id).await?.len(),
        1,
        "only the rogue event remains; no fan-out events were written"
    );
    assert!(
        store.outbox_rows()?.is_empty(),
        "no outbox rows on a conflicting append"
    );
    Ok(())
}

/// An empty fan-out is a no-op: no events, no outbox rows, head unchanged.
#[tokio::test]
async fn fan_out_dispatch_empty_items_is_a_no_op() -> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(5);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());

    recorder
        .record_fan_out_dispatch(recorded_at(1), &[])
        .await?;

    assert_eq!(recorder.current_head(), 0);
    assert!(store.read_history(&workflow_id).await?.is_empty());
    assert!(store.outbox_rows()?.is_empty());
    Ok(())
}

fn activity_failure(message: &str) -> aion_core::ActivityError {
    aion_core::ActivityError {
        kind: aion_core::ActivityErrorKind::Terminal,
        message: String::from(message),
        details: None,
    }
}

/// Count the terminal events (`ActivityCompleted` + `ActivityFailed`) for `ordinal` in `history`.
fn terminal_count(history: &[Event], ordinal: u64) -> usize {
    let target = aion_core::ActivityId::from_sequence_position(ordinal);
    history
        .iter()
        .filter(|event| match event {
            Event::ActivityCompleted { activity_id, .. }
            | Event::ActivityFailed { activity_id, .. } => *activity_id == target,
            _ => false,
        })
        .count()
}

/// (a) First completion for an un-resolved ordinal records the terminal and advances the head by 1.
#[tokio::test]
async fn fan_out_completion_records_first_terminal_for_unresolved_ordinal()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(20);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());

    let result = recorder
        .record_fan_out_completion(
            recorded_at(1),
            0,
            None,
            FanOutOutcome::Completed {
                result: payload("done")?,
                attempt: 1,
            },
        )
        .await?;

    assert_eq!(result, FanOutCompletionResult::Recorded);
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(history.len(), 1, "exactly one terminal event recorded");
    assert_eq!(history[0].seq(), 1);
    match &history[0] {
        Event::ActivityCompleted {
            activity_id,
            result,
            ..
        } => {
            assert_eq!(
                activity_id,
                &aion_core::ActivityId::from_sequence_position(0)
            );
            assert_eq!(result, &payload("done")?);
        }
        other => return Err(format!("expected ActivityCompleted, got {other:?}").into()),
    }
    assert_eq!(recorder.current_head(), 1, "head advances by exactly 1");
    Ok(())
}

/// (b) The core dedup invariant: a duplicate completion for an already-resolved ordinal returns
/// `Dropped`, writes NO second terminal, and leaves the head unchanged.
#[tokio::test]
async fn fan_out_completion_drops_duplicate_for_resolved_ordinal()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(21);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());

    let first = recorder
        .record_fan_out_completion(
            recorded_at(1),
            7,
            None,
            FanOutOutcome::Completed {
                result: payload("first")?,
                attempt: 1,
            },
        )
        .await?;
    assert_eq!(first, FanOutCompletionResult::Recorded);
    let head_after_first = recorder.current_head();

    // A redelivered duplicate completion for the same ordinal must be dropped.
    let duplicate = recorder
        .record_fan_out_completion(
            recorded_at(2),
            7,
            None,
            FanOutOutcome::Completed {
                result: payload("second")?,
                attempt: 1,
            },
        )
        .await?;

    assert_eq!(duplicate, FanOutCompletionResult::Dropped);
    assert_eq!(
        recorder.current_head(),
        head_after_first,
        "a dropped duplicate must not advance the head"
    );
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(
        terminal_count(&history, 7),
        1,
        "no second terminal may be written for an already-resolved ordinal"
    );
    Ok(())
}

/// (b2) A late completion for an ALREADY-CANCELLED ordinal must be dropped, not recorded over the
/// cancellation — `ActivityCancelled` is a terminal in `recorded_terminal`, so the dedup predicate
/// must treat it as resolved.
#[tokio::test]
async fn fan_out_completion_drops_for_cancelled_ordinal() -> Result<(), Box<dyn std::error::Error>>
{
    let workflow_id = workflow_id(22);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());

    recorder
        .record_activity_cancelled(
            recorded_at(1),
            aion_core::ActivityId::from_sequence_position(3),
            1,
        )
        .await?;
    let head_after_cancel = recorder.current_head();

    let result = recorder
        .record_fan_out_completion(
            recorded_at(2),
            3,
            None,
            FanOutOutcome::Completed {
                result: payload("late")?,
                attempt: 1,
            },
        )
        .await?;

    assert_eq!(result, FanOutCompletionResult::Dropped);
    assert_eq!(
        recorder.current_head(),
        head_after_cancel,
        "a dropped completion must not advance the head past the cancellation"
    );
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(
        terminal_count(&history, 3),
        0,
        "no completion terminal may be written over an already-cancelled ordinal"
    );
    Ok(())
}

/// (c) A `Failed` outcome records an `ActivityFailed` terminal carrying the error and attempt.
#[tokio::test]
async fn fan_out_completion_records_failed_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(22);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());

    let result = recorder
        .record_fan_out_completion(
            recorded_at(1),
            3,
            None,
            FanOutOutcome::Failed {
                error: activity_failure("boom"),
                attempt: 2,
            },
        )
        .await?;

    assert_eq!(result, FanOutCompletionResult::Recorded);
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(history.len(), 1);
    match &history[0] {
        Event::ActivityFailed {
            activity_id,
            error,
            attempt,
            ..
        } => {
            assert_eq!(
                activity_id,
                &aion_core::ActivityId::from_sequence_position(3)
            );
            assert_eq!(error, &activity_failure("boom"));
            assert_eq!(*attempt, 2);
        }
        other => return Err(format!("expected ActivityFailed, got {other:?}").into()),
    }
    assert_eq!(recorder.current_head(), 1);
    Ok(())
}

/// (d) Out-of-order: an ordinal that is `Scheduled`+`Started` but not yet terminal is NOT resolved,
/// so its completion records normally.
#[tokio::test]
async fn fan_out_completion_records_for_scheduled_but_not_terminal_ordinal()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(23);
    let store = Arc::new(OutboxTestStore::new());
    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());

    // Stage the dispatch: ordinal 0 is Scheduled+Started (seqs 1,2) but has no terminal yet.
    recorder
        .record_fan_out_dispatch(recorded_at(1), &fan_out_items(0, 1)?)
        .await?;
    assert_eq!(recorder.current_head(), 2);

    let result = recorder
        .record_fan_out_completion(
            recorded_at(2),
            0,
            None,
            FanOutOutcome::Completed {
                result: payload("done")?,
                attempt: 1,
            },
        )
        .await?;

    assert_eq!(
        result,
        FanOutCompletionResult::Recorded,
        "a scheduled+started-but-not-terminal ordinal is not resolved"
    );
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(history.len(), 3, "Scheduled, Started, then Completed");
    assert_eq!(history[2].seq(), 3, "terminal lands at head+1");
    assert!(matches!(history[2], Event::ActivityCompleted { .. }));
    assert_eq!(recorder.current_head(), 3);
    Ok(())
}

/// (e) A sequence conflict surfaces as a hard error WITHOUT advancing the head (mirrors the
/// dispatch-path conflict behaviour). A rogue writer advances the real head behind the recorder.
#[tokio::test]
async fn fan_out_completion_sequence_conflict_surfaces_without_advancing()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow_id = workflow_id(24);
    let store = Arc::new(OutboxTestStore::new());
    // Rogue writer advances the real head to 1; the recorder still expects 0.
    let rogue = Event::WorkflowStarted {
        envelope: aion_core::EventEnvelope {
            seq: 1,
            recorded_at: recorded_at(1),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: String::from("checkout"),
        input: payload("rogue")?,
        run_id: RunId::new(uuid::Uuid::from_u128(1)),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    };
    store
        .append(WriteToken::recorder(), &workflow_id, &[rogue], 0)
        .await?;

    let mut recorder = Recorder::new(workflow_id.clone(), store.clone());
    // The rogue WorkflowStarted is not a terminal for ordinal 0, so dedup passes and the append is
    // attempted with expected_seq 0 against a real head of 1 -> SequenceConflict.
    let result = recorder
        .record_fan_out_completion(
            recorded_at(2),
            0,
            None,
            FanOutOutcome::Completed {
                result: payload("done")?,
                attempt: 1,
            },
        )
        .await;

    match result {
        Err(DurabilityError::Store(StoreError::SequenceConflict { expected, found })) => {
            assert_eq!(expected, 0);
            assert_eq!(found, 1);
        }
        Err(other) => return Err(format!("expected sequence conflict, got {other:?}").into()),
        Ok(value) => return Err(format!("expected sequence conflict, got Ok({value:?})").into()),
    }
    assert_eq!(
        recorder.current_head(),
        0,
        "conflict must not advance the head"
    );
    assert_eq!(
        store.read_history(&workflow_id).await?.len(),
        1,
        "only the rogue event remains; no terminal was written"
    );
    Ok(())
}
