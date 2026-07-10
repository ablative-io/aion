//! Workflow-terminal outbox settlement scenarios (#253) for outbox-bearing stores.
//!
//! Generic over the concrete store (not `Arc<dyn EventStore>`) because the
//! contract under test spans BOTH the [`OutboxStore`] surface (settle twin,
//! enumeration, stale probe, re-arm pins) and the [`WritableEventStore`]
//! writer seam (`settle_workflow_outbox_rows_cancelled`,
//! `rearm_outbox_pending`), which no single trait object carries.

use chrono::{Duration, Utc};

use crate::{OutboxRow, OutboxStore, StoreError, WorkflowId, WritableEventStore};

use super::contract_error;

fn pending_row(workflow_id: &WorkflowId, ordinal: u64) -> Result<OutboxRow, StoreError> {
    Ok(OutboxRow::pending(
        workflow_id.clone(),
        ordinal,
        String::from("charge"),
        aion_core::Payload::from_json(&serde_json::json!({ "ordinal": ordinal }))
            .map_err(|error| StoreError::Serialization(error.to_string()))?,
        Utc::now(),
    ))
}

/// Append one pending row and immediately claim it (it is the only claimable
/// row at that instant, so the claim is deterministic).
async fn append_and_claim<S>(
    store: &S,
    workflow_id: &WorkflowId,
    ordinal: u64,
) -> Result<OutboxRow, StoreError>
where
    S: OutboxStore + WritableEventStore,
{
    store
        .append_outbox_batch(&[pending_row(workflow_id, ordinal)?])
        .await?;
    let claimed = store.claim_outbox_rows(1).await?;
    match claimed.into_iter().next() {
        Some(row) if row.ordinal == ordinal && &row.workflow_id == workflow_id => Ok(row),
        other => Err(contract_error(&format!(
            "expected to claim the just-appended row (ordinal {ordinal}), got {other:?}"
        ))),
    }
}

/// The core settle contract: only the target workflow's live (Pending|Claimed)
/// rows flip to Cancelled; Done/Failed rows and other workflows' rows are
/// untouched; the settled keys come back; the settle is idempotent; and the
/// unsettled-workflow enumeration reflects it.
pub(super) async fn settle_flips_only_live_rows_and_is_idempotent<S>(
    store: S,
) -> Result<(), StoreError>
where
    S: OutboxStore + WritableEventStore,
{
    let dead = super::workflow_id();
    let live = super::workflow_id();

    // dead:0 → Done, dead:1 → Failed (terminal rows the settle must not touch).
    let done = append_and_claim(&store, &dead, 0).await?;
    store.complete_outbox_row(&done.dispatch_key).await?;
    let failed = append_and_claim(&store, &dead, 1).await?;
    store.fail_outbox_row(&failed.dispatch_key).await?;
    // dead:2 → Claimed, dead:3/4 → Pending, live:0 → Pending.
    let claimed = append_and_claim(&store, &dead, 2).await?;
    store
        .append_outbox_batch(&[
            pending_row(&dead, 3)?,
            pending_row(&dead, 4)?,
            pending_row(&live, 0)?,
        ])
        .await?;

    let unsettled = store.list_unsettled_outbox_workflow_ids().await?;
    for workflow in [&dead, &live] {
        if !unsettled.contains(workflow) {
            return Err(contract_error(
                "both workflows own live rows, so both must enumerate as unsettled",
            ));
        }
    }

    let mut settled = store.cancel_outbox_rows_for_workflow(&dead).await?;
    settled.sort();
    let mut expected = vec![
        claimed.dispatch_key.clone(),
        OutboxRow::dispatch_key_for(&dead, 3),
        OutboxRow::dispatch_key_for(&dead, 4),
    ];
    expected.sort();
    super::expect_eq(
        settled,
        expected,
        "the settle must return exactly the live (Pending|Claimed) keys it retired",
    )?;

    // Idempotent: a second settle finds nothing live.
    super::expect_empty(
        store.cancel_outbox_rows_for_workflow(&dead).await?,
        "a second settle of the same workflow must retire nothing",
    )?;

    // The other workflow's row is untouched and still the ONLY claimable row.
    let claimable = store.claim_outbox_rows(16).await?;
    super::expect_eq(
        claimable
            .iter()
            .map(|row| row.dispatch_key.clone())
            .collect::<Vec<_>>(),
        vec![OutboxRow::dispatch_key_for(&live, 0)],
        "after the settle only the live workflow's row may be claimable — \
         Cancelled/Done/Failed rows must never be claimed",
    )?;

    // Enumeration now sees only the (re-claimed) live workflow.
    let unsettled = store.list_unsettled_outbox_workflow_ids().await?;
    super::expect_eq(
        unsettled,
        vec![live],
        "after the settle only the live workflow may own unsettled rows",
    )
}

/// The stale-claim probe is read-only and selects exactly what the re-arm
/// would take.
pub(super) async fn stale_probe_is_readonly_and_matches_rearm_selection<S>(
    store: S,
) -> Result<(), StoreError>
where
    S: OutboxStore + WritableEventStore,
{
    let workflow = super::workflow_id();
    let first = append_and_claim(&store, &workflow, 0).await?;
    let second = append_and_claim(&store, &workflow, 1).await?;
    // Both claims happened "now"; probe with a threshold in the future so both
    // are stale relative to it.
    let older_than = Utc::now() + Duration::hours(1);

    let keys_of = |rows: &[OutboxRow]| {
        rows.iter()
            .map(|row| row.dispatch_key.clone())
            .collect::<Vec<_>>()
    };
    let probed = store.list_stale_claimed_outbox_rows(older_than, 16).await?;
    let probed_again = store.list_stale_claimed_outbox_rows(older_than, 16).await?;
    super::expect_eq(
        keys_of(&probed),
        keys_of(&probed_again),
        "the stale probe must be read-only: probing twice must observe the same rows",
    )?;

    let rearmed = store
        .rearm_stale_claimed_outbox_rows(older_than, Utc::now(), 16)
        .await?;
    let mut rearmed_keys = keys_of(&rearmed);
    rearmed_keys.sort();
    let mut expected = vec![first.dispatch_key, second.dispatch_key];
    expected.sort();
    let mut selected_keys = keys_of(&probed);
    selected_keys.sort();
    super::expect_eq(
        selected_keys,
        expected.clone(),
        "the probe must select exactly the stale claimed rows",
    )?;
    super::expect_eq(
        rearmed_keys,
        expected,
        "the re-arm must take exactly the probe's selection",
    )?;
    super::expect_empty(
        store.list_stale_claimed_outbox_rows(older_than, 16).await?,
        "after the re-arm no stale claimed row may remain",
    )
}

/// Pin: neither the stale re-arm nor the claim path ever touches a Cancelled
/// row — the contract the reconciler's settle-then-rearm ordering depends on.
pub(super) async fn rearm_and_claim_never_touch_cancelled_rows<S>(
    store: S,
) -> Result<(), StoreError>
where
    S: OutboxStore + WritableEventStore,
{
    let workflow = super::workflow_id();
    let _claimed = append_and_claim(&store, &workflow, 0).await?;
    let settled = store.cancel_outbox_rows_for_workflow(&workflow).await?;
    super::expect_eq(
        settled,
        vec![OutboxRow::dispatch_key_for(&workflow, 0)],
        "the claimed row must settle to Cancelled",
    )?;

    super::expect_empty(
        store
            .rearm_stale_claimed_outbox_rows(Utc::now() + Duration::hours(1), Utc::now(), 16)
            .await?,
        "the stale re-arm must never resurrect a Cancelled row",
    )?;
    super::expect_empty(
        store.claim_outbox_rows(16).await?,
        "the claim path must never claim a Cancelled row",
    )
}

/// Reopen interplay (#253-I9): `rearm_outbox_pending` — the reopen/recovery
/// re-stage — forcibly returns ANY existing row, including a Cancelled one, to
/// Pending, so a reopened workflow's re-dispatches still deliver after its
/// earlier terminal settled them.
pub(super) async fn reopen_rearm_resurrects_a_cancelled_row<S>(store: S) -> Result<(), StoreError>
where
    S: OutboxStore + WritableEventStore,
{
    let workflow = super::workflow_id();
    store
        .append_outbox_batch(&[pending_row(&workflow, 0)?])
        .await?;
    let settled = store.cancel_outbox_rows_for_workflow(&workflow).await?;
    super::expect_eq(
        settled,
        vec![OutboxRow::dispatch_key_for(&workflow, 0)],
        "the pending row must settle to Cancelled",
    )?;

    store
        .rearm_outbox_pending(&[pending_row(&workflow, 0)?])
        .await?;
    let claimed = store.claim_outbox_rows(16).await?;
    super::expect_eq(
        claimed
            .iter()
            .map(|row| row.dispatch_key.clone())
            .collect::<Vec<_>>(),
        vec![OutboxRow::dispatch_key_for(&workflow, 0)],
        "rearm_outbox_pending must resurrect the Cancelled row to claimable Pending \
         (reopen supersedes the terminal settle)",
    )
}

/// The writer seam (`WritableEventStore::settle_workflow_outbox_rows_cancelled`,
/// the Recorder's hook) shares the [`OutboxStore`] twin's exact semantics.
pub(super) async fn writer_seam_settle_matches_the_outbox_twin<S>(
    store: S,
) -> Result<(), StoreError>
where
    S: OutboxStore + WritableEventStore,
{
    let workflow = super::workflow_id();
    store
        .append_outbox_batch(&[pending_row(&workflow, 0)?, pending_row(&workflow, 1)?])
        .await?;

    let mut settled = store
        .settle_workflow_outbox_rows_cancelled(&workflow)
        .await?;
    settled.sort();
    let mut expected = vec![
        OutboxRow::dispatch_key_for(&workflow, 0),
        OutboxRow::dispatch_key_for(&workflow, 1),
    ];
    expected.sort();
    super::expect_eq(
        settled,
        expected,
        "the writer-seam settle must retire the workflow's live rows and return their keys",
    )?;
    super::expect_empty(
        store.claim_outbox_rows(16).await?,
        "rows settled through the writer seam must never be claimable",
    )?;
    super::expect_empty(
        store
            .settle_workflow_outbox_rows_cancelled(&workflow)
            .await?,
        "the writer-seam settle must be idempotent",
    )
}
