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
///
/// Aliased to [`aion_core::DEFAULT_TASK_QUEUE`] so the outbox-row default cannot drift from the
/// canonical domain task-queue default; both the namespace and task-queue fallbacks resolve to the
/// same `"default"` literal.
pub const DEFAULT_OUTBOX_ROUTE: &str = aion_core::DEFAULT_TASK_QUEUE;

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

/// Pool scope for a node-affinity-aware outbox claim (LSUB-1a).
///
/// A scope restricts a claim to the rows servable by one worker pool: the `(namespace, task_queue)`
/// the pool serves, plus the optional `node` locality of the claiming node. It is the additive,
/// opt-in counterpart to the unscoped [`OutboxStore::claim_outbox_rows`] — passing no scope keeps
/// the legacy single-server behaviour of claiming any visible row.
///
/// # Node predicate
///
/// `node` is the *claiming node's* id, not a row filter that demands an exact match. A row is in
/// scope for node `N` when its own `node` affinity is **either** `Some(N)` (explicitly pinned to
/// `N`) **or** `None` (unpinned — no affinity, servable by any node in the pool). Rows pinned to a
/// *different* node `Some(M)` where `M != N` are excluded.
///
/// This matches the NODE-AFFINITY model where `node` on a row is OPTIONAL locality
/// ([`OutboxRow::node`]): unpinned rows (`None`) are the genuine current behaviour — claimable by
/// anyone in the pool — so a node-scoped claim must keep serving them, otherwise enabling affinity
/// for some rows would silently strand every unpinned row. A `node: None` scope (a pool that
/// advertises no locality) claims only unpinned rows, never another node's pinned rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimScope {
    /// Namespace the pool serves; only rows with this exact `namespace` are in scope.
    pub namespace: String,
    /// Task queue the pool serves; only rows with this exact `task_queue` are in scope.
    pub task_queue: String,
    /// Claiming node's locality id, or `None` for a pool that advertises no node affinity.
    ///
    /// `Some(n)` claims rows with `node == Some(n)` AND unpinned rows (`node == None`). `None` claims
    /// only unpinned rows (`node == None`).
    pub node: Option<String>,
}

impl ClaimScope {
    /// Builds a scope for the `(namespace, task_queue)` pool with no node locality.
    #[must_use]
    pub fn new(namespace: impl Into<String>, task_queue: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            task_queue: task_queue.into(),
            node: None,
        }
    }

    /// Sets the claiming node's locality id on this scope.
    #[must_use]
    pub fn with_node(mut self, node: Option<String>) -> Self {
        self.node = node;
        self
    }

    /// Returns whether `row` is servable under this scope.
    ///
    /// True iff the namespace and task queue match exactly AND the node predicate holds: the row is
    /// unpinned (`node == None`) or pinned to this scope's node (`row.node == self.node` when
    /// `self.node` is `Some`). See the [type docs](ClaimScope#node-predicate) for the rationale.
    #[must_use]
    pub fn admits(&self, row: &OutboxRow) -> bool {
        row.namespace == self.namespace
            && row.task_queue == self.task_queue
            && match (&self.node, &row.node) {
                // Unpinned rows are servable by any node in the pool.
                (_, None) => true,
                // A pinned row is servable only by the node it is pinned to.
                (Some(scope_node), Some(row_node)) => scope_node == row_node,
                // A pool with no locality cannot serve another node's pinned row.
                (None, Some(_)) => false,
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

    /// Atomically claims up to `limit` pending rows that are due AND in `scope` (LSUB-1a).
    ///
    /// This is the node-affinity-aware counterpart to [`OutboxStore::claim_outbox_rows`]: it adds a
    /// `(namespace, task_queue, node)` predicate to the same atomic, single-writer claim and is
    /// otherwise byte-identical (same due/order/limit/claim semantics). The unscoped method is left
    /// exactly as it was — passing no scope is still "claim any visible row" — so the existing
    /// single-server poll loop is unaffected.
    ///
    /// A row is in scope when its `namespace` and `task_queue` match `scope` exactly and the node
    /// predicate holds: the row is unpinned (`node == None`, servable by any node in the pool) or
    /// pinned to `scope.node`. See [`ClaimScope`] for the full node-predicate rationale.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a stored row cannot be decoded.
    async fn claim_outbox_rows_scoped(
        &self,
        scope: &ClaimScope,
        limit: u32,
    ) -> Result<Vec<OutboxRow>, StoreError>;

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

    /// Returns the count of in-flight outbox rows for `namespace` (CP2-Q1.5).
    ///
    /// "In-flight" is the dispatched-but-not-terminal set: rows whose `status` is
    /// [`OutboxStatus::Pending`] OR [`OutboxStatus::Claimed`]. Terminal rows
    /// ([`OutboxStatus::Done`], [`OutboxStatus::Failed`], [`OutboxStatus::Cancelled`]) are excluded.
    ///
    /// This is the durable, restart-correct quota source that replaces the in-memory
    /// `inflight_activities` gauge proven dead in P2-Q0 (see `docs/design/CONTROL-PLANE-PHASE-2.md`
    /// §3.3/§8). Because it counts durable rows, the count survives a restart, and because a
    /// `Claimed` row is in-flight, a row that dispatched but whose `mark_done` failed (the
    /// stuck-`Claimed` case) is still counted — it has not reached a terminal outcome and the worker
    /// may still be running it. The count is strictly scoped to `namespace`: rows in any other
    /// namespace are never included.
    ///
    /// Nothing consumes this yet (P2-Q2 will); it is a pure additive store query with no behaviour
    /// change.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a stored row cannot be decoded.
    async fn count_inflight_outbox_rows(&self, namespace: &str) -> Result<u64, StoreError>;

    /// Returns the count of CLAIMED outbox rows for `namespace` (CP2-Q2).
    ///
    /// "Claimed" is the *concurrently executing* set: rows in [`OutboxStatus::Claimed`] — dispatched
    /// to a worker and not yet terminal. This is deliberately NARROWER than
    /// [`OutboxStore::count_inflight_outbox_rows`], which also counts [`OutboxStatus::Pending`]
    /// backlog: a tenant sitting on a large Pending backlog has a large *in-flight* count but a small
    /// *claimed* count, and it is the CLAIMED count — concurrent executing activities — that the
    /// keyed-backpressure ceiling caps (CP-Phase-2 §3.1 as corrected). Counting Pending+Claimed for
    /// headroom would wedge a tenant against its own backlog: it could never claim the Pending rows
    /// that make up the count. So headroom is `per_node_ceiling − claimed`, never `… − inflight`.
    ///
    /// A stuck-`Claimed` row (dispatched but `mark_done` never landed, `outbox_dispatcher` §) is
    /// still `Claimed` and so still counts — the worker may still be executing it, so it correctly
    /// occupies a concurrency slot. The count is strictly scoped to `namespace`: rows in any other
    /// namespace are never included.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a stored row cannot be decoded.
    async fn count_claimed_outbox_rows(&self, namespace: &str) -> Result<u64, StoreError>;

    /// Counts the CLAIMED outbox rows for each namespace in `namespaces`, in ONE pass (CP2-Q2 perf).
    ///
    /// Same semantics as calling [`OutboxStore::count_claimed_outbox_rows`] once per namespace — the
    /// CLAIMED-only ([`OutboxStatus::Claimed`]), owned-shard-scoped concurrent-executing count that
    /// feeds the keyed-backpressure headroom — but collapsed into a single scan of the owned-shard
    /// set instead of N repeated scans over the same rows (the N+1 the per-sweep planner would
    /// otherwise incur, one full scan per active namespace). The returned map has EXACTLY one entry
    /// per requested namespace: a namespace with no claimed rows maps to `0`, so the caller can index
    /// it unconditionally. Namespaces not in `namespaces` are never counted (nor returned).
    ///
    /// The default implementation preserves the contract by delegating to the per-namespace method
    /// (an honest, correct fallback for any store that has not specialised the single-scan form); the
    /// bundled stores override it with a genuine one-pass scan / grouped query.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a stored row cannot be decoded.
    async fn count_claimed_outbox_rows_by_namespace(
        &self,
        namespaces: &[&str],
    ) -> Result<std::collections::BTreeMap<String, u64>, StoreError> {
        let mut counts = std::collections::BTreeMap::new();
        for namespace in namespaces {
            let count = self.count_claimed_outbox_rows(namespace).await?;
            counts.insert((*namespace).to_owned(), count);
        }
        Ok(counts)
    }

    /// Enumerates the distinct `(namespace, task_queue, node)` routes that currently have at least
    /// one CLAIMABLE pending row — a row whose `status` is [`OutboxStatus::Pending`] and whose
    /// `visible_after` fence has passed (CP2-Q2).
    ///
    /// This is the enumeration primitive the keyed-backpressure dispatcher round-robins over: it
    /// cannot ask [`OutboxStore::claim_outbox_rows_scoped`] (which needs a *specific*
    /// [`ClaimScope`]) to "claim across all namespaces", so it first probes which routes have work
    /// and then issues one scoped, headroom-capped claim per route. Each returned [`ClaimScope`]
    /// carries the exact `(namespace, task_queue, node)` of pending rows, so a subsequent
    /// `claim_outbox_rows_scoped` with that scope claims those rows (and any unpinned rows in the
    /// same pool — see [`ClaimScope`]). A route with only future-fenced (`visible_after > now`) or
    /// terminal rows is NOT returned: there is nothing claimable to dispatch.
    ///
    /// The probe is read-only and claims nothing; it only shapes which scopes the dispatcher then
    /// claims under. On a node that owns a shard subset, only routes with claimable rows on owned
    /// shards are returned (the same owned-shard scoping as the claim path), so the per-node round
    /// naturally sees only its proportional slice of each tenant's work.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] for backend boundary failures and
    /// [`StoreError::Serialization`] when a stored row cannot be decoded.
    async fn pending_outbox_routes(&self) -> Result<Vec<ClaimScope>, StoreError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{ContentType, Payload, WorkflowId};
    use chrono::Utc;

    use super::{ClaimScope, OutboxRow, OutboxStatus, OutboxStore};

    #[test]
    fn outbox_store_is_object_safe() {
        let _: Option<Arc<dyn OutboxStore>> = None;
    }

    fn row(namespace: &str, task_queue: &str, node: Option<&str>) -> OutboxRow {
        OutboxRow::pending(
            WorkflowId::new_v4(),
            0,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now(),
        )
        .with_namespace(namespace)
        .with_task_queue(task_queue)
        .with_node(node.map(ToOwned::to_owned))
    }

    #[test]
    fn scope_admits_matching_namespace_task_queue_and_unpinned_or_matching_node() {
        let scope = ClaimScope::new("remote", "gpu").with_node(Some("box-7".to_owned()));
        // Pinned to the scope's node: admitted.
        assert!(scope.admits(&row("remote", "gpu", Some("box-7"))));
        // Unpinned (no affinity): admitted by any node in the pool.
        assert!(scope.admits(&row("remote", "gpu", None)));
    }

    #[test]
    fn scope_rejects_other_namespace_task_queue_or_pinned_to_other_node() {
        let scope = ClaimScope::new("remote", "gpu").with_node(Some("box-7".to_owned()));
        // Wrong namespace.
        assert!(!scope.admits(&row("default", "gpu", None)));
        // Wrong task queue.
        assert!(!scope.admits(&row("remote", "cpu", None)));
        // Pinned to a different node.
        assert!(!scope.admits(&row("remote", "gpu", Some("box-9"))));
    }

    #[test]
    fn node_less_scope_admits_only_unpinned_rows() {
        let scope = ClaimScope::new("remote", "gpu");
        assert!(scope.admits(&row("remote", "gpu", None)));
        // A node-less pool cannot serve a row pinned to a specific node.
        assert!(!scope.admits(&row("remote", "gpu", Some("box-7"))));
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
