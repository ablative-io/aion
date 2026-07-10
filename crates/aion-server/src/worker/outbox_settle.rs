//! Terminal-workflow outbox settlement sweep (#253).
//!
//! The durable outbox table — not any in-memory cache — is the source of truth
//! for what may still be delivered, so it must be kept consistent with workflow
//! terminality. The Recorder settles a workflow's live rows at terminal-record
//! time (the live window); this module is the BOOT/ADOPTION backstop for the
//! restart window the incident fell through: a workflow that reached a durable
//! terminal without its rows being settled (a settle-hook store error, or a
//! node that died between the terminal append and the settle) must have those
//! rows retired BEFORE the dispatcher's first claim, or a redialed worker
//! serves a full zombie round for a dead workflow.
//!
//! Liveness is projected from event history with the SAME projection
//! `list_active` and pause validation use ([`status_from_events`]) — NOT
//! `list_active` membership, which would wrongly classify `Paused` runs (live,
//! merely held) as dead. Only the four hard terminals settle; `ContinuedAsNew`
//! never reaches the settle set because a continued chain's later
//! `WorkflowStarted` projects the chain `Running`, and a chain whose
//! replacement run has not started yet is still in flight.

use aion_core::{WorkflowStatus, status_from_events};
use aion_store::{EventStore, OutboxStore, StoreError};
use tracing::info;

/// Whether `status` is a settle-eligible workflow terminal: the four hard
/// terminals, never `ContinuedAsNew` (the workflow continues) and never
/// `Paused`/`Running` (live).
#[must_use]
pub fn is_settle_terminal(status: WorkflowStatus) -> bool {
    matches!(
        status,
        WorkflowStatus::Completed
            | WorkflowStatus::Failed
            | WorkflowStatus::Cancelled
            | WorkflowStatus::TimedOut
    )
}

/// Settle every terminal workflow's live outbox rows to `Cancelled`, returning
/// the settled `dispatch_key`s (#253).
///
/// Enumerates the distinct workflow ids owning any `Pending`/`Claimed` row,
/// projects each workflow's status ONCE from its full recorded history, and
/// retires the rows of workflows whose projected status is a hard terminal.
/// Bounded cost: proportional to workflows with live rows; runs once per boot
/// (before the dispatcher's first claim) and once per shard adoption (after
/// the fence widened the owned scope).
///
/// # Errors
///
/// Returns [`StoreError`] when the enumeration, a history read, or a settle
/// fails; the caller logs and continues (the settle-at-terminal hook and the
/// stale-claim reconciler gate remain as repair paths).
pub async fn settle_terminal_outbox_rows(
    event_store: &dyn EventStore,
    outbox_store: &dyn OutboxStore,
) -> Result<Vec<String>, StoreError> {
    let workflow_ids = outbox_store.list_unsettled_outbox_workflow_ids().await?;
    let mut settled = Vec::new();
    for workflow_id in workflow_ids {
        let history = event_store.read_history(&workflow_id).await?;
        let status = status_from_events(&history);
        if !is_settle_terminal(status) {
            continue;
        }
        let keys = outbox_store
            .cancel_outbox_rows_for_workflow(&workflow_id)
            .await?;
        if !keys.is_empty() {
            info!(
                workflow_id = %workflow_id,
                projected_status = ?status,
                settled = keys.len(),
                dispatch_keys = ?keys,
                "settled outbox rows for terminal workflow"
            );
            settled.extend(keys);
        }
    }
    Ok(settled)
}

#[cfg(test)]
mod tests {
    use super::is_settle_terminal;
    use aion_core::WorkflowStatus;

    #[test]
    fn only_hard_terminals_are_settle_eligible() {
        assert!(is_settle_terminal(WorkflowStatus::Completed));
        assert!(is_settle_terminal(WorkflowStatus::Failed));
        assert!(is_settle_terminal(WorkflowStatus::Cancelled));
        assert!(is_settle_terminal(WorkflowStatus::TimedOut));
        // Live states: Running plainly, Paused is live-but-held (#204), and a
        // ContinuedAsNew projection means the replacement run has not started
        // yet — the chain is still in flight.
        assert!(!is_settle_terminal(WorkflowStatus::Running));
        assert!(!is_settle_terminal(WorkflowStatus::Paused));
        assert!(!is_settle_terminal(WorkflowStatus::ContinuedAsNew));
    }
}
