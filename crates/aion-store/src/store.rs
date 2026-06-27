//! Event-store traits and single-writer capability.

use aion_core::{
    Event, RunId, TimerId, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{OutboxRow, StoreError, TimerEntry};

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

    /// Restrict every per-workflow enumeration (active workflows, timers, outbox
    /// rows) to the named set of distribution shards this node owns, or restore
    /// the own-all-shards default when `shards` is `None`.
    ///
    /// This is the engine-lifecycle hook behind a multi-shard deployment: the
    /// boot path tells the store which shards this node serves so recovery and
    /// enumeration see only that node's slice of the cluster's state. The
    /// default implementation is a deliberate no-op — single-shard backends
    /// (in-memory, libSQL) own everything unconditionally, so a `None` or any
    /// shard set leaves their behaviour byte-identical. Only a sharded backend
    /// (haematite) overrides this to scope its enumeration. Decorators that wrap
    /// another store must forward this call to their inner store.
    fn set_owned_shards(&self, shards: Option<&[usize]>) {
        let _ = shards;
    }

    /// Acquire-and-serve ownership of each named distribution shard BEFORE the
    /// boot path recovers or enumerates over them, so the node is the fenced
    /// owner and its replicated state is union-merged locally first.
    ///
    /// This is the SS-2 election hook the engine boot path calls right after
    /// [`Self::set_owned_shards`] and BEFORE startup recovery: a distributed
    /// backend wins the per-shard election and becomes the live owner, so the
    /// subsequent recovery reads see the full committed history for its shards.
    ///
    /// The default implementation is a deliberate no-op returning `Ok(())` —
    /// single-shard / non-distributed backends (in-memory, libSQL, and the
    /// single-node haematite mode) own everything unconditionally and elect
    /// nothing, so boot stays byte-identical. Only a DISTRIBUTED sharded backend
    /// overrides this to run the election. Decorators that wrap another store
    /// must forward this call to their inner store.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when a distributed backend cannot win the
    /// election or become the live owner of one of `shards`; the node must not
    /// serve those shards in that case (fail-closed).
    fn acquire_owned_shards(&self, shards: &[usize]) -> Result<(), StoreError> {
        let _ = shards;
        Ok(())
    }
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

    /// Atomically appends `events` and the durable-outbox `outbox_rows` for `workflow_id` in a
    /// single transaction, under the same expected-head sequence guard as [`Self::append`].
    ///
    /// This is the durable fan-out write: the `ActivityScheduled`/`ActivityStarted` scheduling
    /// events and their matching outbox rows commit together or not at all. Atomicity is
    /// load-bearing — a committed fan-out batch always carries both, or neither — so the out-of-band
    /// dispatcher can never observe an outbox row whose scheduling events were rolled back, and a
    /// re-issued append cannot leave events without their dispatch rows.
    ///
    /// # Default implementation (safe for outbox-unaware backends)
    ///
    /// The default delegates to [`Self::append`] when `outbox_rows` is empty (byte-for-byte
    /// equivalent to an event-only append), and otherwise returns [`StoreError::Backend`] **rather
    /// than silently dropping the outbox rows**. Dropping them would be the dangerous failure mode:
    /// the events would commit, the workflow would believe its fan-out is durably staged, and the
    /// rows would never be dispatched. A hard error forces a backend to opt in to durable-outbox
    /// support by overriding this method (as the libSQL store does) before any caller can route a
    /// fan-out batch through it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SequenceConflict`] when the stored head differs from `expected_seq`,
    /// [`StoreError::Serialization`] when an event or outbox payload cannot be serialized, and
    /// [`StoreError::Backend`] for backend boundary failures or when an outbox-unaware backend is
    /// asked to persist a non-empty `outbox_rows` slice.
    async fn append_with_outbox(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[OutboxRow],
    ) -> Result<(), StoreError> {
        if outbox_rows.is_empty() {
            return self.append(token, workflow_id, events, expected_seq).await;
        }
        Err(StoreError::Backend(String::from(
            "this event store does not support durable-outbox appends; \
             refusing to drop outbox rows (override WritableEventStore::append_with_outbox)",
        )))
    }

    /// Returns the outbox rows for `rows`' `dispatch_key`s to `Pending`, re-staging them for the
    /// out-of-band dispatcher.
    ///
    /// This is the crash-recovery re-arm: on first arrival after a restart, an activity whose
    /// `ActivityScheduled` is recorded but which has no terminal event lost its in-flight dispatch
    /// when the previous engine process died. Under the durable-outbox model the recovering workflow
    /// re-stages the dispatch by flipping its outbox row back to claimable `Pending` (an UPSERT — a
    /// brand-new `dispatch_key` with no prior row is inserted as `Pending`) instead of driving an
    /// in-process completion task. Redelivery is safe: the completion dedup
    /// (`record_fan_out_completion`) ignores a terminal for an already-resolved ordinal, so re-arm is
    /// at-least-once.
    ///
    /// The dispatch retry budget is preserved across re-arm: a backend must NOT reset an existing
    /// row's `attempt` to zero, so a workflow that reliably crashes the server still eventually
    /// dead-letters rather than re-dispatching forever.
    ///
    /// # Default implementation (safe for outbox-unaware backends)
    ///
    /// An empty `rows` slice is `Ok(())`. A non-empty slice returns [`StoreError::Backend`] **rather
    /// than silently no-op'ing the re-arm**: a store without durable-outbox support cannot re-stage a
    /// dispatch, and silently dropping the request would strand the recovered activity. A hard error
    /// forces a backend to opt in (as the libSQL store does) before any caller can route a re-arm
    /// through it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Serialization`] when an outbox payload cannot be serialized, and
    /// [`StoreError::Backend`] for backend boundary failures or when an outbox-unaware backend is
    /// asked to re-arm a non-empty `rows` slice.
    async fn rearm_outbox_pending(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        Err(StoreError::Backend(String::from(
            "this event store does not support durable-outbox re-arm; \
             refusing to drop a non-empty re-arm (override WritableEventStore::rearm_outbox_pending)",
        )))
    }

    /// Idempotently settles one outbox row to cancelled when this writer is backed by an outbox.
    ///
    /// Outbox-aware backends override this and delegate to [`crate::OutboxStore`]. The default is a
    /// no-op so non-outbox test stores and legacy backends can still record `ActivityCancelled`
    /// history without requiring an outbox table.
    ///
    /// # Errors
    ///
    /// Outbox-aware overrides return [`StoreError::Backend`] for backend boundary failures.
    async fn settle_outbox_row_cancelled(&self, dispatch_key: &str) -> Result<(), StoreError> {
        let _ = dispatch_key;
        Ok(())
    }
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
