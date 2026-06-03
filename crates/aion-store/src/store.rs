//! `EventStore` trait.

use aion_core::{Event, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{StoreError, TimerEntry};

/// Durable event-history, visibility-query, and timer contract for Aion stores.
#[async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Atomically appends `events` to `workflow_id` when the stored history head equals
    /// `expected_seq`.
    ///
    /// Implementations must apply every event in `events` or none of them. If the current stored
    /// head for `workflow_id` differs from `expected_seq`, this method must return
    /// [`StoreError::SequenceConflict`] and leave history unchanged. A first append with
    /// `expected_seq == 0` creates the workflow history implicitly.
    ///
    /// The store does not invent event sequences: the caller supplies already-enveloped events,
    /// and `expected_seq` is the optimistic-concurrency guard for the append.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SequenceConflict`] when the stored head differs from `expected_seq`.
    async fn append(
        &self,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError>;

    /// Reads the complete event history for `workflow_id` in ascending sequence order.
    ///
    /// A workflow with no recorded events is observed as an empty history. This includes unknown
    /// workflow identifiers: because the first append with `expected_seq == 0` creates a workflow
    /// implicitly, "unknown workflow" and "empty history" are the same observable state for reads.
    /// This method must not return [`StoreError::NotFound`] for absent workflows.
    ///
    /// # Errors
    ///
    /// Returns backend or serialization errors encountered while reading history.
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError>;

    /// Lists workflow identifiers whose projected status is non-terminal.
    ///
    /// Implementations derive activeness from authoritative event history, not from an independent
    /// mutable status field. Empty stores return an empty list, and this method must not return
    /// [`StoreError::NotFound`] for absent workflows.
    ///
    /// # Errors
    ///
    /// Returns backend or serialization errors encountered while projecting active workflows.
    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError>;

    /// Returns workflow summaries matching `filter`.
    ///
    /// Summaries are projections from workflow histories. Filters that match no workflows, including
    /// because no workflow exists, return an empty vector. This method must not return
    /// [`StoreError::NotFound`] for absent workflows.
    ///
    /// # Errors
    ///
    /// Returns backend or serialization errors encountered while querying workflow summaries.
    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError>;

    /// Persists a durable timer for `workflow_id` that is due at `fire_at`.
    ///
    /// Scheduling a timer is part of the same logical transaction as appending the corresponding
    /// `TimerStarted` event: an engine should consider the timer scheduled only when both the
    /// history append and durable timer record succeed.
    ///
    /// # Errors
    ///
    /// Returns backend or serialization errors encountered while persisting the timer record.
    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// Returns durable timers whose `fire_at` is less than or equal to `as_of`.
    ///
    /// Implementations may return the entries in any deterministic order appropriate for the
    /// backend, but every returned [`TimerEntry`] must be due as of the supplied recorded instant.
    ///
    /// # Errors
    ///
    /// Returns backend or serialization errors encountered while reading durable timer records.
    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::EventStore;

    #[test]
    fn event_store_is_object_safe() {
        let _: Option<Arc<dyn EventStore>> = None;
    }
}
