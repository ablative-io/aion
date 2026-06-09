//! Event-store traits and single-writer capability.

use aion_core::{
    Event, RunId, TimerId, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{StoreError, TimerEntry};

mod write_capability {
    /// Capability required to append workflow events.
    ///
    /// This token enforces Aion's single-writer durability invariant at the type level: only the
    /// recorder append path may hold write authority for a workflow. `SequenceConflict` remains the
    /// runtime defense-in-depth signal for any internal misuse or future bypass that attempts to
    /// append with a stale head.
    #[derive(Clone, Copy, Debug)]
    pub struct WriteToken {
        _private: (),
    }

    impl WriteToken {
        /// Constructs a write token for Aion's recorder path.
        #[must_use]
        pub fn recorder() -> Self {
            Self { _private: () }
        }
    }

    pub(crate) fn conformance() -> WriteToken {
        WriteToken { _private: () }
    }
}

pub use write_capability::WriteToken;

/// Summary of one concrete run in a workflow's continuation chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    /// Concrete run identifier for this chain entry.
    pub run_id: RunId,
    /// Parent run that continued as this run, or `None` for the first run.
    pub parent_run_id: Option<RunId>,
    /// Status projected from this run's slice of lifecycle events.
    pub status: WorkflowStatus,
    /// Timestamp of this run's `WorkflowStarted` event.
    pub started_at: DateTime<Utc>,
    /// Timestamp of this run's terminal lifecycle event, when closed.
    pub closed_at: Option<DateTime<Utc>>,
}

/// Read and durable-timer contract for Aion event stores.
#[async_trait]
pub trait ReadableEventStore: Send + Sync + 'static {
    /// Reads the complete event history for `workflow_id` in ascending sequence order.
    ///
    /// A workflow with no recorded events is observed as an empty history. This includes unknown
    /// workflow identifiers: because the first append with `expected_seq == 0` creates a workflow
    /// implicitly, "unknown workflow" and "empty history" are the same observable state for reads.
    /// This method must not return [`StoreError::NotFound`] for absent workflows.
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError>;

    /// Reads the concrete run chain for `workflow_id` in continuation order.
    async fn read_run_chain(&self, workflow_id: &WorkflowId)
    -> Result<Vec<RunSummary>, StoreError>;

    /// Lists workflow identifiers whose projected status is non-terminal.
    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError>;

    /// Returns workflow summaries matching `filter`.
    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError>;

    /// Persists a durable timer for `workflow_id` that is due at `fire_at`.
    ///
    /// Timer scheduling remains on the public store surface because timers are not workflow-history
    /// appends and are used by the timer subsystem after the recorder has written `TimerStarted`.
    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// Returns durable timers whose `fire_at` is less than or equal to `as_of`.
    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError>;
}

/// Write authority for appending workflow-history events.
///
/// `append` requires a [`WriteToken`], so having an `Arc<dyn EventStore>` or
/// `Arc<dyn ReadableEventStore>` is not sufficient to write events.
#[async_trait]
pub trait WritableEventStore: Send + Sync + 'static {
    /// Atomically appends `events` to `workflow_id` when the stored history head equals
    /// `expected_seq`.
    ///
    /// Implementations must apply every event in `events` or none of them. If the current stored
    /// head for `workflow_id` differs from `expected_seq`, this method must return
    /// [`StoreError::SequenceConflict`] and leave history unchanged. A first append with
    /// `expected_seq == 0` creates the workflow history implicitly.
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError>;
}

/// Convenience trait for concrete stores that support both reads/timers and recorder writes.
pub trait EventStore: ReadableEventStore + WritableEventStore {}

impl<T> EventStore for T where T: ReadableEventStore + WritableEventStore + ?Sized {}

pub(crate) fn conformance_write_token() -> WriteToken {
    write_capability::conformance()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{EventStore, ReadableEventStore, WritableEventStore};

    #[test]
    fn event_store_traits_are_object_safe() {
        let _: Option<Arc<dyn ReadableEventStore>> = None;
        let _: Option<Arc<dyn WritableEventStore>> = None;
        let _: Option<Arc<dyn EventStore>> = None;
    }
}
