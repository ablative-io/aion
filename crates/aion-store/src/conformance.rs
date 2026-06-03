//! Reusable behavioural conformance suite for [`EventStore`] implementations.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aion_core::{
    Event, EventEnvelope, Payload, TimerId, WorkflowError, WorkflowFilter, WorkflowId,
    WorkflowStatus, WorkflowSummary,
};
use chrono::{DateTime, Utc};

use crate::{EventStore, StoreError, TimerEntry};

/// Runs the shared behavioural suite for an [`EventStore`] implementation.
///
/// The supplied factory is called once per scenario and must return a fresh, isolated store. Durable
/// backends should create a new temporary database or namespace each time the factory is invoked.
///
/// # Errors
///
/// Returns the first store error or contract assertion failure encountered by the suite.
pub async fn run_event_store_suite<MakeStore, MakeFuture>(
    make_store: MakeStore,
) -> Result<(), StoreError>
where
    MakeStore: Fn() -> MakeFuture,
    MakeFuture: Future<Output = Arc<dyn EventStore>>,
{
    append_and_read_history_round_trip(make_store().await).await?;
    multi_batch_append_advances_sequence(make_store().await).await?;
    stale_expected_sequence_writes_nothing(make_store().await).await?;
    list_active_reflects_projected_status(make_store().await).await?;
    query_applies_all_filters(make_store().await).await?;
    expired_timers_include_due_boundary_and_exclude_future(make_store().await).await?;
    rescheduling_same_timer_replaces_prior_fire_at(make_store().await).await?;
    Ok(())
}

async fn append_and_read_history_round_trip(store: Arc<dyn EventStore>) -> Result<(), StoreError> {
    let workflow_id = workflow_id();
    expect_empty(
        store.read_history(&workflow_id).await?,
        "unknown workflow history should be empty",
    )?;

    let events = vec![
        workflow_started(1, &workflow_id, "checkout")?,
        activity_scheduled(2, &workflow_id, "reserve-inventory")?,
        timer_started(3, &workflow_id, TimerId::anonymous(3), recorded_at(30)?)?,
        signal_received(4, &workflow_id, "payment-authorized")?,
    ];

    store.append(&workflow_id, &events, 0).await?;

    expect_eq(
        store.read_history(&workflow_id).await?,
        events,
        "read_history should round-trip appended events in ascending sequence order",
    )
}

async fn multi_batch_append_advances_sequence(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let workflow_id = workflow_id();
    let first = workflow_started(1, &workflow_id, "checkout")?;
    let second_batch = vec![
        activity_scheduled(2, &workflow_id, "charge-card")?,
        workflow_completed(3, &workflow_id)?,
    ];

    store
        .append(&workflow_id, std::slice::from_ref(&first), 0)
        .await?;
    store.append(&workflow_id, &second_batch, 1).await?;

    let mut expected = vec![first];
    expected.extend(second_batch);
    expect_eq(
        store.read_history(&workflow_id).await?,
        expected,
        "append should accept the current head as the next expected sequence",
    )
}

async fn stale_expected_sequence_writes_nothing(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let workflow_id = workflow_id();
    let first = workflow_started(1, &workflow_id, "checkout")?;
    let rejected = vec![workflow_completed(2, &workflow_id)?];

    store
        .append(&workflow_id, std::slice::from_ref(&first), 0)
        .await?;
    let conflict = store.append(&workflow_id, &rejected, 0).await;

    expect_eq(
        conflict,
        Err(StoreError::SequenceConflict {
            expected: 0,
            found: 1,
        }),
        "stale expected_seq should report the observed workflow head",
    )?;
    expect_eq(
        store.read_history(&workflow_id).await?,
        vec![first],
        "SequenceConflict should leave history unchanged with no partial write",
    )
}

async fn list_active_reflects_projected_status(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    expect_empty(
        store.list_active().await?,
        "empty store should have no active workflows",
    )?;

    let running = workflow_id();
    let completing = workflow_id();

    store
        .append(&running, &[workflow_started(1, &running, "checkout")?], 0)
        .await?;
    store
        .append(
            &completing,
            &[workflow_started(1, &completing, "billing")?],
            0,
        )
        .await?;

    expect_contains_exactly(
        store.list_active().await?,
        &[running.clone(), completing.clone()],
        "started workflows should be listed as active before terminal events",
    )?;

    store
        .append(&completing, &[workflow_completed(2, &completing)?], 1)
        .await?;

    expect_contains_exactly(
        store.list_active().await?,
        &[running],
        "terminal workflow lifecycle events should remove workflows from active listing",
    )
}

async fn query_applies_all_filters(store: Arc<dyn EventStore>) -> Result<(), StoreError> {
    let running_checkout = workflow_id();
    let completed_checkout = workflow_id();
    let failed_billing = workflow_id();
    let parent = workflow_id();

    store
        .append(
            &running_checkout,
            &[workflow_started_at(1, &running_checkout, "checkout", 1)?],
            0,
        )
        .await?;
    store
        .append(
            &completed_checkout,
            &[
                workflow_started_at(1, &completed_checkout, "checkout", 10)?,
                workflow_completed_at(2, &completed_checkout, 11)?,
            ],
            0,
        )
        .await?;
    store
        .append(
            &failed_billing,
            &[
                workflow_started_at(1, &failed_billing, "billing", 20)?,
                workflow_failed_at(2, &failed_billing, 21)?,
            ],
            0,
        )
        .await?;

    let summaries = store.query(&WorkflowFilter::default()).await?;
    let expected_workflows = [
        running_checkout.clone(),
        completed_checkout.clone(),
        failed_billing,
    ];
    expect_summary_workflows(
        summaries,
        &expected_workflows,
        "default query filter should match every workflow with a start event",
    )?;

    let completed_checkout_filter = WorkflowFilter {
        workflow_type: Some(String::from("checkout")),
        status: Some(WorkflowStatus::Completed),
        started_after: Some(recorded_at(10)?),
        started_before: Some(recorded_at(10)?),
        parent: None,
    };
    let summaries = store.query(&completed_checkout_filter).await?;
    expect_eq(
        summaries.len(),
        1,
        "combined type/status/time-range query should require all filters to match inclusively",
    )?;
    let summary = summaries
        .first()
        .ok_or_else(|| contract_error("query should return the completed checkout summary"))?;
    expect_eq(
        summary.workflow_id.clone(),
        completed_checkout,
        "query should return the workflow matching all requested filters",
    )?;
    expect_eq(
        summary.workflow_type.clone(),
        String::from("checkout"),
        "query summary should preserve workflow type",
    )?;
    expect_eq(
        summary.status,
        WorkflowStatus::Completed,
        "query summary should project status from history",
    )?;
    expect_eq(
        summary.started_at,
        recorded_at(10)?,
        "query summary should use WorkflowStarted recorded_at as started_at",
    )?;
    expect_eq(
        summary.ended_at,
        Some(recorded_at(11)?),
        "query summary should use terminal event recorded_at as ended_at",
    )?;

    let parent_filter = WorkflowFilter {
        parent: Some(parent),
        ..WorkflowFilter::default()
    };
    expect_empty(
        store.query(&parent_filter).await?,
        "parent filters should return no workflows when no summary carries that parent",
    )
}

async fn expired_timers_include_due_boundary_and_exclude_future(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let workflow_id = workflow_id();
    let past_timer = TimerId::anonymous(1);
    let boundary_timer = TimerId::anonymous(2);
    let future_timer = TimerId::anonymous(3);
    let as_of = recorded_at(20)?;

    store
        .schedule_timer(&workflow_id, &future_timer, recorded_at(30)?)
        .await?;
    store
        .schedule_timer(&workflow_id, &boundary_timer, as_of)
        .await?;
    store
        .schedule_timer(&workflow_id, &past_timer, recorded_at(10)?)
        .await?;

    expect_timer_entries(
        store.expired_timers(as_of).await?,
        &[
            TimerEntry {
                workflow_id: workflow_id.clone(),
                timer_id: past_timer,
                fire_at: recorded_at(10)?,
            },
            TimerEntry {
                workflow_id,
                timer_id: boundary_timer,
                fire_at: as_of,
            },
        ],
        "expired_timers should include fire_at <= as_of and exclude future timers",
    )
}

async fn rescheduling_same_timer_replaces_prior_fire_at(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let workflow_id = workflow_id();
    let timer_id = TimerId::anonymous(1);
    let first_fire_at = recorded_at(10)?;
    let replacement_fire_at = recorded_at(30)?;

    store
        .schedule_timer(&workflow_id, &timer_id, first_fire_at)
        .await?;
    store
        .schedule_timer(&workflow_id, &timer_id, replacement_fire_at)
        .await?;

    expect_empty(
        store.expired_timers(first_fire_at).await?,
        "rescheduling the same timer should replace its earlier fire_at",
    )?;
    expect_timer_entries(
        store.expired_timers(replacement_fire_at).await?,
        &[TimerEntry {
            workflow_id,
            timer_id,
            fire_at: replacement_fire_at,
        }],
        "rescheduled timer should expire at the replacement fire_at",
    )
}

fn recorded_at(offset_seconds: i64) -> Result<DateTime<Utc>, StoreError> {
    DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0)
        .ok_or_else(|| contract_error("test timestamp should be representable"))
}

fn workflow_id() -> WorkflowId {
    static NEXT_WORKFLOW_ID: AtomicU64 = AtomicU64::new(1);

    WorkflowId::new(uuid::Uuid::from_u128(u128::from(
        NEXT_WORKFLOW_ID.fetch_add(1, Ordering::Relaxed),
    )))
}

fn envelope(seq: u64, workflow_id: &WorkflowId) -> Result<EventEnvelope, StoreError> {
    let offset = i64::try_from(seq).map_err(|error| {
        StoreError::Backend(format!("event sequence out of timestamp range: {error}"))
    })?;
    envelope_at(seq, workflow_id, offset)
}

fn envelope_at(
    seq: u64,
    workflow_id: &WorkflowId,
    offset_seconds: i64,
) -> Result<EventEnvelope, StoreError> {
    Ok(EventEnvelope {
        seq,
        recorded_at: recorded_at(offset_seconds)?,
        workflow_id: workflow_id.clone(),
    })
}

fn payload(label: &str) -> Result<Payload, StoreError> {
    Payload::from_json(&serde_json::json!({ "label": label }))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

fn workflow_started(
    seq: u64,
    workflow_id: &WorkflowId,
    workflow_type: &str,
) -> Result<Event, StoreError> {
    Ok(Event::WorkflowStarted {
        envelope: envelope(seq, workflow_id)?,
        workflow_type: workflow_type.to_owned(),
        input: payload("input")?,
    })
}

fn workflow_started_at(
    seq: u64,
    workflow_id: &WorkflowId,
    workflow_type: &str,
    offset_seconds: i64,
) -> Result<Event, StoreError> {
    Ok(Event::WorkflowStarted {
        envelope: envelope_at(seq, workflow_id, offset_seconds)?,
        workflow_type: workflow_type.to_owned(),
        input: payload("input")?,
    })
}

fn workflow_completed(seq: u64, workflow_id: &WorkflowId) -> Result<Event, StoreError> {
    workflow_completed_at(
        seq,
        workflow_id,
        i64::try_from(seq).map_err(|error| {
            StoreError::Backend(format!("event sequence out of timestamp range: {error}"))
        })?,
    )
}

fn workflow_completed_at(
    seq: u64,
    workflow_id: &WorkflowId,
    offset_seconds: i64,
) -> Result<Event, StoreError> {
    Ok(Event::WorkflowCompleted {
        envelope: envelope_at(seq, workflow_id, offset_seconds)?,
        result: payload("result")?,
    })
}

fn workflow_failed_at(
    seq: u64,
    workflow_id: &WorkflowId,
    offset_seconds: i64,
) -> Result<Event, StoreError> {
    Ok(Event::WorkflowFailed {
        envelope: envelope_at(seq, workflow_id, offset_seconds)?,
        error: WorkflowError {
            message: String::from("failed"),
            details: None,
        },
    })
}

fn activity_scheduled(
    seq: u64,
    workflow_id: &WorkflowId,
    activity_type: &str,
) -> Result<Event, StoreError> {
    Ok(Event::ActivityScheduled {
        envelope: envelope(seq, workflow_id)?,
        activity_id: aion_core::ActivityId::from_sequence_position(seq),
        activity_type: activity_type.to_owned(),
        input: payload("activity-input")?,
    })
}

fn timer_started(
    seq: u64,
    workflow_id: &WorkflowId,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
) -> Result<Event, StoreError> {
    Ok(Event::TimerStarted {
        envelope: envelope(seq, workflow_id)?,
        timer_id,
        fire_at,
    })
}

fn signal_received(seq: u64, workflow_id: &WorkflowId, name: &str) -> Result<Event, StoreError> {
    Ok(Event::SignalReceived {
        envelope: envelope(seq, workflow_id)?,
        name: name.to_owned(),
        payload: payload("signal")?,
    })
}

fn expect_empty<T>(items: Vec<T>, message: &str) -> Result<(), StoreError> {
    let len = items.len();
    drop(items);

    if len == 0 {
        Ok(())
    } else {
        Err(contract_error(&format!("{message} (got {len} items)")))
    }
}

fn expect_eq<T>(actual: T, expected: T, message: &str) -> Result<(), StoreError>
where
    T: PartialEq + std::fmt::Debug,
{
    if actual == expected {
        drop((actual, expected));
        Ok(())
    } else {
        Err(contract_error(&format!(
            "{message}\n  expected: {expected:?}\n  actual: {actual:?}"
        )))
    }
}

fn expect_contains_exactly(
    mut actual: Vec<WorkflowId>,
    expected: &[WorkflowId],
    message: &str,
) -> Result<(), StoreError> {
    actual.sort_by_key(ToString::to_string);
    let mut expected = expected.to_vec();
    expected.sort_by_key(ToString::to_string);
    expect_eq(actual, expected, message)
}

fn expect_summary_workflows(
    actual: Vec<WorkflowSummary>,
    expected: &[WorkflowId],
    message: &str,
) -> Result<(), StoreError> {
    let actual = actual
        .into_iter()
        .map(|summary| summary.workflow_id)
        .collect::<Vec<_>>();
    expect_contains_exactly(actual, expected, message)
}

fn expect_timer_entries(
    mut actual: Vec<TimerEntry>,
    expected: &[TimerEntry],
    message: &str,
) -> Result<(), StoreError> {
    actual.sort_by(timer_entry_order);
    let mut expected = expected.to_vec();
    expected.sort_by(timer_entry_order);
    expect_eq(actual, expected, message)
}

fn timer_entry_order(left: &TimerEntry, right: &TimerEntry) -> std::cmp::Ordering {
    left.fire_at
        .cmp(&right.fire_at)
        .then_with(|| {
            left.workflow_id
                .to_string()
                .cmp(&right.workflow_id.to_string())
        })
        .then_with(|| left.timer_id.to_string().cmp(&right.timer_id.to_string()))
}

fn contract_error(message: &str) -> StoreError {
    StoreError::Backend(format!("event store conformance failure: {message}"))
}
