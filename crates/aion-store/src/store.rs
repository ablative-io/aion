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

    /// Reads the event history for `workflow_id` restricted to events with sequence number
    /// greater than or equal to `from_seq`, in ascending sequence order.
    ///
    /// This is the range-read primitive behind O(delta) WS resume: callers replaying from a
    /// cursor must not pay for the full history. Semantics:
    ///
    /// - `from_seq <= 1` is equivalent to [`Self::read_history`]: sequence numbers start at 1,
    ///   so every recorded event satisfies the bound.
    /// - `from_seq` beyond the current head returns an empty vector, never an error. Whether a
    ///   beyond-head cursor is *valid* is protocol judgment, not store judgment: the WS resume
    ///   protocol rejects `resume_from_seq > head + 1` as an invalid cursor
    ///   (`ResumeCursorAheadOfHistory`), but it makes that call by comparing the cursor against
    ///   the head it observes — the store only answers which events exist at or after the
    ///   requested sequence.
    /// - Unknown workflows behave exactly like [`Self::read_history`] for unknown workflows:
    ///   empty history, never [`StoreError::NotFound`], because "unknown workflow" and "empty
    ///   history" are the same observable state for reads.
    ///
    /// There is deliberately no default implementation: a read-all-then-filter fallback would
    /// silently reintroduce O(history) behavior. Every backend must implement this as a real
    /// range read (for SQL backends, an indexed `seq >= ?` range scan).
    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError>;

    /// Reads the concrete run chain for `workflow_id` in continuation order.
    async fn read_run_chain(&self, workflow_id: &WorkflowId)
    -> Result<Vec<RunSummary>, StoreError>;

    /// Lists every workflow identifier that has at least one event in history.
    ///
    /// Unlike [`Self::list_active`], this includes terminal workflows and exists to let projection
    /// repair jobs reconcile derived indexes against the authoritative event history.
    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError>;

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

/// Convenience trait for concrete stores that support reads/timers, recorder
/// writes, and deployed-package persistence.
///
/// [`crate::PackageStore`] is part of the contract, not an optional add-on:
/// runtime-deployed packages share the durability promise of event history
/// (a recovered run is pinned to a recorded package version, and a backend
/// that dropped the archive would strand it).
pub trait EventStore: ReadableEventStore + WritableEventStore + crate::PackageStore {}

impl<T> EventStore for T where
    T: ReadableEventStore + WritableEventStore + crate::PackageStore + ?Sized
{
}

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
