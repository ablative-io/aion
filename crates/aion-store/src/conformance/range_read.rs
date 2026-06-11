//! Range-read (`read_history_from`) scenarios for the event-store conformance suite.

use std::sync::Arc;

use super::{
    activity_scheduled, expect_empty, expect_eq, signal_received, workflow_completed, workflow_id,
    workflow_started,
};
use crate::{Event, EventStore, StoreError, WorkflowId};

pub(super) async fn middle_of_history_returns_ordered_suffix(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let (workflow_id, events) = appended_four_event_history(&store).await?;

    expect_eq(
        store.read_history_from(&workflow_id, 3).await?,
        events[2..].to_vec(),
        "read_history_from in the middle of history should return the ordered suffix seq >= from_seq",
    )
}

pub(super) async fn from_seq_one_matches_full_read(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let (workflow_id, events) = appended_four_event_history(&store).await?;

    expect_eq(
        store.read_history_from(&workflow_id, 1).await?,
        store.read_history(&workflow_id).await?,
        "read_history_from with from_seq = 1 should be equivalent to read_history",
    )?;
    expect_eq(
        store.read_history_from(&workflow_id, 1).await?,
        events,
        "read_history_from with from_seq = 1 should return the complete ordered history",
    )
}

pub(super) async fn beyond_head_returns_empty_not_error(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let (workflow_id, events) = appended_four_event_history(&store).await?;
    let head = events.len() as u64;

    expect_empty(
        store.read_history_from(&workflow_id, head + 1).await?,
        "read_history_from one past the head should return empty, not an error",
    )?;
    expect_empty(
        store.read_history_from(&workflow_id, head + 100).await?,
        "read_history_from far beyond the head should return empty, not an error",
    )
}

pub(super) async fn unknown_workflow_matches_full_read_semantics(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let unknown = workflow_id();

    expect_eq(
        store.read_history_from(&unknown, 1).await?,
        store.read_history(&unknown).await?,
        "read_history_from for an unknown workflow should match read_history (empty, no error)",
    )?;
    expect_empty(
        store.read_history_from(&unknown, 5).await?,
        "read_history_from for an unknown workflow should be empty for any from_seq",
    )
}

pub(super) async fn single_event_history_boundaries(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let workflow_id = workflow_id();
    let only_event = workflow_started(1, &workflow_id, "checkout")?;

    store
        .append(
            crate::store::conformance_write_token(),
            &workflow_id,
            std::slice::from_ref(&only_event),
            0,
        )
        .await?;

    expect_eq(
        store.read_history_from(&workflow_id, 1).await?,
        vec![only_event],
        "read_history_from at a single-event history's only sequence should return that event",
    )?;
    expect_empty(
        store.read_history_from(&workflow_id, 2).await?,
        "read_history_from past a single-event history should return empty",
    )
}

pub(super) async fn from_seq_at_head_returns_only_head_event(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let (workflow_id, events) = appended_four_event_history(&store).await?;
    let head = events.len() as u64;
    let head_event = events
        .last()
        .cloned()
        .ok_or_else(|| super::contract_error("four-event fixture should have a head event"))?;

    expect_eq(
        store.read_history_from(&workflow_id, head).await?,
        vec![head_event],
        "read_history_from at the head should return exactly the head event",
    )
}

async fn appended_four_event_history(
    store: &Arc<dyn EventStore>,
) -> Result<(WorkflowId, Vec<Event>), StoreError> {
    let workflow_id = workflow_id();
    let events = vec![
        workflow_started(1, &workflow_id, "checkout")?,
        activity_scheduled(2, &workflow_id, "reserve-inventory")?,
        signal_received(3, &workflow_id, "payment-authorized")?,
        workflow_completed(4, &workflow_id)?,
    ];

    store
        .append(
            crate::store::conformance_write_token(),
            &workflow_id,
            &events,
            0,
        )
        .await?;

    Ok((workflow_id, events))
}
