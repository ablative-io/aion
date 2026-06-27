//! Durable outbox contract for store-backed fan-out dispatch.
//!
//! The outbox is a transactional staging table written in the same atomic batch as the
//! workflow-history events that schedule fan-out activities (see
//! [`crate::WritableEventStore::append`] and the libSQL `append_with_outbox` path). A separate,
//! non-replayed dispatcher claims pending rows, dispatches them to connected workers, and marks
//! them done or schedules a retry. This module declares only the storage contract; the dispatcher
//! and Recorder wiring live outside the store crate.
//!
//! Idempotency is enforced at the database level: each row carries a `dispatch_key`
//! (`"{workflow_id}:{ordinal}"`) under a `UNIQUE` constraint, so a re-issued append of the same
//! fan-out batch silently ignores the duplicate rows rather than dispatching them twice.

use aion_core::{Payload, RunId, WorkflowId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::StoreError;

/// Routing identity a row carries when no explicit value was staged: the `"default"` namespace and
/// the `"default"` task queue. This is both the fresh-staging fallback (no SDK task-queue selection
/// exists yet — NSTQ-4) and the legacy-NULL read-back value for rows persisted before the columns
/// existed (NSTQ-2).
pub const DEFAULT_OUTBOX_ROUTE: &str = "default";

/// Lifecycle state of an outbox row as the dispatcher drives it to a terminal outcome.
///
/// Rows are inserted `Pending`, transitioned to `Claimed` while a dispatcher holds them, and end
/// in `Done` (dispatched and acknowledged) or `Failed` (retry budget exhausted). `Failed` is a
/// dead-letter marker for operator inspection; the dispatcher never re-claims it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxStatus {
    /// Awaiting a dispatcher claim once `visible_after` has passed.
    Pending,
    /// Claimed by a dispatcher and in flight.
    Claimed,
    /// Dispatched and acknowledged; terminal.
    Done,
    /// Retry budget exhausted; terminal dead letter.
    Failed,
    /// Cancelled by workflow history before dispatch completed; terminal.
    Cancelled,
}

impl OutboxStatus {
    /// Returns the canonical lowercase token persisted in the `status` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses a persisted `status` token back into an [`OutboxStatus`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Serialization`] when `value` is not one of the four canonical tokens.
    pub fn parse_token(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "claimed" => Ok(Self::Claimed),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(StoreError::Serialization(format!(
                "unknown outbox status: {other}"
            ))),
        }
    }
}

/// One durable fan-out dispatch staged for a worker.
///
/// The row carries everything the out-of-band dispatcher needs to send the activity without
/// reading workflow history: the originating workflow, the pinned `ordinal` within its fan-out
/// range, the derived `dispatch_key` idempotency guard, the activity type, and the input payload.
/// `attempt`, `visible_after`, `claimed_at`, and `status` track retry/backoff and claim state.
/// `claimed_at` is set only while a row is [`OutboxStatus::Claimed`]; pending and terminal rows
/// keep it `None` so stale-claim reconciliation only considers durable claimed rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRow {
    /// Database-level idempotency key, canonically `"{workflow_id}:{ordinal}"`.
    pub dispatch_key: String,
    /// Workflow that scheduled this fan-out activity.
    pub workflow_id: WorkflowId,
    /// Pinned ordinal of this activity within the workflow's fan-out range.
    pub ordinal: u64,
    /// Run that dispatched this ordinal; `None` for legacy rows (pre-RunId threading). Threaded so a
    /// completion only resolves the run that issued it (continue-as-new safety, OBX-011).
    pub run_id: Option<RunId>,
    /// Workflow's durable isolation namespace — the correctness boundary the dispatched activity must
    /// route within. Legacy rows (pre-NSTQ-2, persisted before the column existed) read back as the
    /// `"default"` namespace. Carried on the row so the dispatcher routes via the workflow's real
    /// namespace instead of inventing the server default (NSTQ-2).
    pub namespace: String,
    /// Pool/flavour selector within the namespace. There is no SDK-level task-queue selection yet
    /// (NSTQ-4), so a freshly staged row carries the named `"default"` task queue; legacy rows
    /// (pre-NSTQ-2) also read back as `"default"`. Carried on the row so the dispatcher routes via the
    /// row's real selector (NSTQ-2).
    pub task_queue: String,
    /// OPTIONAL locality affinity within the `(namespace, task_queue)` pool. `None` = no affinity =
    /// any worker in the pool (the genuine current behaviour: there is no SDK-level node selection
    /// yet — NODE-4). `Some(node)` pins the dispatch to workers advertising that node id. Legacy
    /// rows (pre-NODE-2, persisted before the column existed) read back as `None`: a NULL column is
    /// "no affinity", NOT a sentinel string (NODE-2).
    pub node: Option<String>,
    /// Activity type the worker must execute.
    pub activity_type: String,
    /// Opaque activity input payload.
    pub input: Payload,
    /// Lifecycle state of this row.
    pub status: OutboxStatus,
    /// Zero-based dispatch attempt count; incremented on each retry.
    pub attempt: u32,
    /// Earliest instant at which this row becomes claimable (retry backoff fence).
    pub visible_after: DateTime<Utc>,
    /// Durable instant at which the row was claimed; absent unless `status` is `Claimed`.
    pub claimed_at: Option<DateTime<Utc>>,
}

impl OutboxRow {
    /// Builds the canonical `dispatch_key` for a `(workflow_id, ordinal)` pair.
    ///
    /// This is the single source of truth for the idempotency key format so the append path and any
    /// completion-routing lookups agree byte-for-byte.
    #[must_use]
    pub fn dispatch_key_for(workflow_id: &WorkflowId, ordinal: u64) -> String {
        format!("{workflow_id}:{ordinal}")
    }

    /// Constructs a fresh `Pending` row for `(workflow_id, ordinal)` with attempt zero.
    ///
    /// `visible_after` is set to `now` so the row is immediately claimable. The `dispatch_key` is
    /// derived via [`OutboxRow::dispatch_key_for`].
    #[must_use]
    pub fn pending(
        workflow_id: WorkflowId,
        ordinal: u64,
        activity_type: String,
        input: Payload,
        now: DateTime<Utc>,
    ) -> Self {
        let dispatch_key = Self::dispatch_key_for(&workflow_id, ordinal);
        Self {
            dispatch_key,
            workflow_id,
            ordinal,
            run_id: None,
            namespace: String::from(DEFAULT_OUTBOX_ROUTE),
            task_queue: String::from(DEFAULT_OUTBOX_ROUTE),
            node: None,
            activity_type,
            input,
            status: OutboxStatus::Pending,
            attempt: 0,
            visible_after: now,
            claimed_at: None,
        }
    }

    /// Sets the dispatching run on this row (the run that owns this ordinal).
    #[must_use]
    pub fn with_run_id(mut self, run_id: Option<RunId>) -> Self {
        self.run_id = run_id;
        self
    }

    /// Sets the workflow's durable isolation namespace on this row (the routing correctness boundary).
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Sets the pool/flavour selector (task queue) on this row.
    #[must_use]
    pub fn with_task_queue(mut self, task_queue: impl Into<String>) -> Self {
        self.task_queue = task_queue.into();
        self
    }

    /// Sets the OPTIONAL node affinity on this row. `None` = no affinity (any worker in the pool).
    #[must_use]
    pub fn with_node(mut self, node: Option<String>) -> Self {
        self.node = node;
        self
    }
}

/// Durable staging and claim contract for store-backed fan-out dispatch.
///
/// Implementations append outbox rows transactionally with workflow-history events, hand pending
/// rows to a single dispatcher under the single-writer model, and record terminal outcomes. All
/// methods are idempotency-aware: appending a duplicate `dispatch_key` is silently ignored, and the
/// completion/retry/fail transitions key off `dispatch_key`.
#[async_trait]
pub trait OutboxStore: Send + Sync + 'static {
    /// Inserts `rows` into the outbox, silently ignoring any whose `dispatch_key` already exists.
    ///
    /// This is the standalone (non-atomic-with-events) append used for tests and out-of-band
    /// staging. The atomic-with-history append lives on the concrete store as `append_with_outbox`.
    /// Duplicate keys are ignored via `INSERT OR IGNORE`, preserving at-most-once dispatch.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a row cannot be encoded.
    async fn append_outbox_batch(&self, rows: &[OutboxRow]) -> Result<(), StoreError>;

    /// Atomically claims up to `limit` pending rows whose `visible_after` has passed.
    ///
    /// Claimed rows are transitioned to [`OutboxStatus::Claimed`] and returned. Under the
    /// single-writer IMMEDIATE model this is the SQLite-equivalent of `SELECT ... FOR UPDATE SKIP
    /// LOCKED`: no two dispatchers observe the same pending row as claimable.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a stored row cannot be decoded.
    async fn claim_outbox_rows(&self, limit: u32) -> Result<Vec<OutboxRow>, StoreError>;

    /// Re-arms stale claimed rows so a live dispatcher can claim them again without restart.
    ///
    /// Implementations atomically select up to `limit` rows whose `status` is
    /// [`OutboxStatus::Claimed`] and whose durable `claimed_at` timestamp is older than
    /// `older_than`, then transition only those rows back to [`OutboxStatus::Pending`] with
    /// `visible_after` set to the supplied instant. The existing `attempt` value is preserved and
    /// `claimed_at` is cleared. Rows in `Done` or `Failed` are terminal and must never be touched.
    /// Rows in `Cancelled` are also terminal and must never be touched.
    ///
    /// Claimed rows without a durable `claimed_at` value are deliberately ignored: the caller asked
    /// for rows older than a supplied instant, and `NULL` cannot satisfy that predicate safely.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a stored row cannot be decoded.
    async fn rearm_stale_claimed_outbox_rows(
        &self,
        older_than: DateTime<Utc>,
        visible_after: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<OutboxRow>, StoreError>;

    /// Marks the row identified by `dispatch_key` as [`OutboxStatus::Done`].
    ///
    /// A `dispatch_key` with no matching row is a no-op (the dedup guard may have removed it), not
    /// an error.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures.
    async fn complete_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError>;

    /// Returns the row identified by `dispatch_key` to [`OutboxStatus::Pending`] for retry.
    ///
    /// Sets `attempt` to `next_attempt` and `visible_after` to `visible_after` so the dispatcher
    /// honours backoff before re-claiming. An absent `dispatch_key` is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures.
    async fn retry_outbox_row(
        &self,
        dispatch_key: &str,
        next_attempt: u32,
        visible_after: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// Marks the row identified by `dispatch_key` as [`OutboxStatus::Failed`] (dead letter).
    ///
    /// An absent `dispatch_key` is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures.
    async fn fail_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{OutboxRow, OutboxStatus, OutboxStore};

    #[test]
    fn outbox_store_is_object_safe() {
        let _: Option<Arc<dyn OutboxStore>> = None;
    }

    #[test]
    fn status_tokens_round_trip() -> Result<(), crate::StoreError> {
        for status in [
            OutboxStatus::Pending,
            OutboxStatus::Claimed,
            OutboxStatus::Done,
            OutboxStatus::Failed,
            OutboxStatus::Cancelled,
        ] {
            let parsed = OutboxStatus::parse_token(status.as_str())?;
            assert_eq!(parsed, status);
        }
        Ok(())
    }

    #[test]
    fn unknown_status_token_is_rejected() {
        assert!(OutboxStatus::parse_token("nope").is_err());
    }

    #[test]
    fn dispatch_key_is_workflow_id_colon_ordinal() {
        let workflow_id = aion_core::WorkflowId::new_v4();
        let key = OutboxRow::dispatch_key_for(&workflow_id, 7);
        assert_eq!(key, format!("{workflow_id}:7"));
    }
}
