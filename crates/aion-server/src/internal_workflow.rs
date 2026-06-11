//! Engine-internal workflow filtering for user-facing enumeration surfaces.
//!
//! The engine runs plumbing workflows of its own — today the schedule
//! coordinator, which durably hosts every schedule's lifecycle events. Those
//! executions live in the same event and visibility stores as user workflows,
//! so without an explicit exclusion they surface in list and count responses.
//! Every user-facing enumeration surface filters through this module: the
//! shared list/count handlers (gRPC `ListWorkflows`/`CountWorkflows` and
//! `POST /workflows/list`) and the HTTP visibility routes (`GET /workflows`,
//! `GET /workflows/count`). `describe` by explicit workflow id intentionally
//! still resolves internal workflows as an operator escape hatch.

use std::sync::Arc;

use aion_store::StoreError;
use aion_store::visibility::{ListWorkflowsFilter, VisibilityStore, WorkflowSummary};

/// Workflow types reserved by the engine for internal plumbing executions.
///
/// The engine names internal workflows under its reserved `aion.` prefix; the
/// schedule coordinator's type constant lives in the `aion` crate
/// (`schedule_coordinator_workflow_type()`) but is not exported, so the wire
/// value is pinned here. New engine-internal workflow types must be added to
/// this list so every enumeration surface keeps hiding them.
const INTERNAL_WORKFLOW_TYPES: &[&str] = &["aion.schedule_coordinator"];

/// Returns whether `workflow_type` is an engine-internal workflow type that
/// must be hidden from user-facing list and count responses.
pub(crate) fn is_internal_workflow_type(workflow_type: &str) -> bool {
    INTERNAL_WORKFLOW_TYPES.contains(&workflow_type)
}

/// Drops engine-internal executions from a list response.
pub(crate) fn retain_user_workflows(summaries: &mut Vec<WorkflowSummary>) {
    summaries.retain(|summary| !is_internal_workflow_type(&summary.workflow_type));
}

/// Counts workflows matching `filter`, excluding engine-internal executions.
///
/// The visibility store cannot express "workflow type is not internal" in a
/// single query, so the internal portion is measured with exact
/// `workflow_type` queries (the internal type set is a closed list) and
/// subtracted. A filter that explicitly targets an internal type counts
/// nothing: enumeration surfaces never acknowledge engine internals.
///
/// # Errors
///
/// Returns store errors from the underlying count queries unchanged.
pub(crate) async fn count_user_workflows(
    store: &Arc<dyn VisibilityStore>,
    filter: ListWorkflowsFilter,
) -> Result<u64, StoreError> {
    if let Some(workflow_type) = &filter.workflow_type {
        if is_internal_workflow_type(workflow_type) {
            return Ok(0);
        }
        return store.count_workflows(filter).await;
    }
    let mut count = store.count_workflows(filter.clone()).await?;
    for internal_type in INTERNAL_WORKFLOW_TYPES {
        let mut internal_filter = filter.clone();
        internal_filter.workflow_type = Some((*internal_type).to_owned());
        count = count.saturating_sub(store.count_workflows(internal_filter).await?);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use aion_core::{RunId, WorkflowId, WorkflowStatus};
    use aion_store::InMemoryStore;
    use aion_store::visibility::VisibilityRecord;
    use chrono::Utc;

    use super::*;

    #[test]
    fn predicate_matches_only_internal_types() {
        assert!(is_internal_workflow_type("aion.schedule_coordinator"));
        assert!(!is_internal_workflow_type("checkout"));
        assert!(!is_internal_workflow_type("schedule_coordinator"));
    }

    fn record(workflow_type: &str, id: u128) -> VisibilityRecord {
        VisibilityRecord {
            workflow_id: WorkflowId::new(uuid::Uuid::from_u128(id)),
            run_id: RunId::new(uuid::Uuid::from_u128(id + 100)),
            workflow_type: workflow_type.to_owned(),
            status: WorkflowStatus::Running,
            start_time: Utc::now(),
            close_time: None,
            search_attributes: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn count_excludes_internal_workflows() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn VisibilityStore> = Arc::new(InMemoryStore::default());
        store.record_visibility(record("checkout", 1)).await?;
        store
            .record_visibility(record("aion.schedule_coordinator", 2))
            .await?;

        let unfiltered = count_user_workflows(&store, ListWorkflowsFilter::default()).await?;
        assert_eq!(unfiltered, 1);

        let user_typed = count_user_workflows(
            &store,
            ListWorkflowsFilter {
                workflow_type: Some(String::from("checkout")),
                ..ListWorkflowsFilter::default()
            },
        )
        .await?;
        assert_eq!(user_typed, 1);

        let internal_typed = count_user_workflows(
            &store,
            ListWorkflowsFilter {
                workflow_type: Some(String::from("aion.schedule_coordinator")),
                ..ListWorkflowsFilter::default()
            },
        )
        .await?;
        assert_eq!(internal_typed, 0);
        Ok(())
    }

    #[test]
    fn retain_drops_internal_summaries() {
        let mut summaries = vec![
            WorkflowSummary::from(record("checkout", 1)),
            WorkflowSummary::from(record("aion.schedule_coordinator", 2)),
        ];
        retain_user_workflows(&mut summaries);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].workflow_type, "checkout");
    }
}
