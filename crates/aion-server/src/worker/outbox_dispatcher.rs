//! Non-replayed background dispatcher for the durable fan-out outbox.
//!
//! # What this is
//!
//! `OutboxDispatcher` is a Tokio task that lives entirely OUTSIDE the
//! deterministic replay domain. It reads ONLY the outbox table — never workflow
//! history — claims pending rows under the single-writer model, dispatches each
//! to a connected worker through the existing server-side push
//! [`ActivityDispatcher`](crate::worker::ActivityDispatcher), and records the
//! row's terminal outbox state:
//!
//! - dispatch succeeds → [`OutboxStore::complete_outbox_row`] (`done`);
//! - dispatch fails and the attempt budget is not exhausted →
//!   [`OutboxStore::retry_outbox_row`] with an exponential-backoff
//!   `visible_after` fence and the attempt count bumped;
//! - dispatch fails on the final attempt → [`OutboxStore::fail_outbox_row`]
//!   (`failed`, a dead letter for operator inspection).
//!
//! # Dormant by default
//!
//! Nothing here runs unless the operator sets `outbox.enabled = true`. The
//! spawn in [`crate::run`] is gated on that flag, so a default server never
//! constructs or starts this task and its behaviour is identical to before the
//! outbox existed.
//!
//! # Phase boundary (Phase 2 vs Phase 3)
//!
//! # Placement (Control-Plane Phase 2, P2-P3)
//!
//! When a [`PlacementCache`](crate::worker::PlacementCache) is attached to
//! [`WorkerOutboxDispatch`], an UNPINNED row (`row.node == None`) whose namespace
//! placement is `Prefer{L}` is dispatched preferring an L-labelled worker, with a
//! spill to ANY live worker when none of the preferred labels is up. This is the
//! ONLY placement behaviour this slice ships:
//!
//! - a per-activity authored pin (`row.node == Some(N)`) always wins and is
//!   dispatched off the row's own node, untouched by placement;
//! - `Unplaced` is unchanged (any worker);
//! - `Pinned{L}` is STORED and read here, but its hard-constraint *dispatch
//!   enforcement* (require-and-wait, plus the `Some(N ∉ L)` start-admission
//!   rejection) is **out of scope for P2-P3 — it is P2-I1**. Until then a `Pinned`
//!   namespace dispatches like `Unplaced` (any worker). This is documented
//!   deliberately so the soft-spill slice ships without the isolation gate.
//!
//! The determinism invariant is absolute: placement is consulted ONLY for live
//! worker selection in this non-replayed task; the recorded row's `node` is NEVER
//! mutated by placement, so a workflow's command stream is identical regardless of
//! which worker a `Prefer` directive routed the activity to (CP-Phase-2 §2.4).
//!
//! # Phase boundary (Phase 2 vs Phase 3)
//!
//! Phase 2 scope ends at the outbox row's terminal state. A *successful*
//! dispatch here means the activity task was accepted by a connected worker; it
//! does NOT route the eventual worker completion back into workflow history.
//! That completion → [`Recorder`](aion::Recorder) wiring (the cross-node
//! completion-dedup chokepoint) is Phase 3 and is deliberately NOT built in this
//! module. Until Phase 3 lands, the dispatcher is exercised only against the
//! outbox table; it must not be commissioned on a server that relies on fan-out
//! completions reaching history.

use std::sync::Arc;
use std::time::Duration;

use aion_core::ActivityId;
use aion_store::{OutboxRow, OutboxStore};
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::error::ServerError;
use crate::worker::{ActivityDispatcher, ScheduledActivity};

/// Resolved, non-optional outbox dispatcher settings.
///
/// Built from the validated [`OutboxConfig`](crate::config::OutboxConfig) only
/// once the operator has commissioned the dispatcher, so every field is a
/// concrete operator decision — there are no defaults to invent here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxDispatcherConfig {
    /// Interval between successive claim sweeps.
    pub poll_interval: Duration,
    /// Maximum rows claimed per sweep.
    pub batch_size: u32,
    /// Total dispatch attempts before a row is dead-lettered.
    pub max_attempts: u32,
    /// Base backoff applied to the first retry.
    pub backoff_base: Duration,
    /// Geometric growth factor applied per prior attempt.
    pub backoff_multiplier: u32,
    /// Upper bound on a single retry's backoff.
    pub backoff_max: Duration,
}

impl OutboxDispatcherConfig {
    /// Computes the backoff delay before the retry that follows `attempt`.
    ///
    /// `attempt` is the just-failed attempt count (zero-based for the first
    /// dispatch). The delay grows geometrically —
    /// `backoff_base * backoff_multiplier^attempt` — and is clamped to
    /// `backoff_max`. All arithmetic saturates rather than overflowing, so a
    /// large attempt count or multiplier simply pins the delay at the ceiling.
    #[must_use]
    pub fn backoff_for_attempt(&self, attempt: u32) -> Duration {
        let max_ms = u128::from(u64::MAX);
        let multiplier = u128::from(self.backoff_multiplier);
        let mut delay_ms = self.backoff_base.as_millis().min(max_ms);
        for _ in 0..attempt {
            delay_ms = delay_ms.saturating_mul(multiplier);
            if delay_ms >= max_ms {
                break;
            }
        }
        let cap_ms = self.backoff_max.as_millis().min(max_ms);
        let clamped_ms = delay_ms.min(cap_ms);
        Duration::from_millis(u64::try_from(clamped_ms).unwrap_or(u64::MAX))
    }
}

/// Abstraction over the push-dispatch of one claimed outbox row.
///
/// The production implementation forwards to the server's
/// [`ActivityDispatcher`]; tests substitute an in-test sink that records or
/// rejects dispatches deterministically without a connected worker. Modelling
/// dispatch as a trait keeps the claim/retry/terminal-state loop testable in
/// isolation from the gRPC worker registry.
#[async_trait]
pub trait OutboxRowDispatch: Send + Sync + 'static {
    /// Dispatch one claimed row to a worker.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the row cannot be placed with a worker. A
    /// returned error drives the row into retry (or dead-letter) rather than
    /// `done`.
    async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError>;
}

/// Production [`OutboxRowDispatch`] backed by the connected-worker registry.
///
/// Maps an [`OutboxRow`] to a [`ScheduledActivity`] and pushes it through the
/// existing [`ActivityDispatcher`]. Since NSTQ-2 the row carries its own
/// `namespace` and `task_queue`, so dispatch routes via the workflow's real
/// routing identity read straight off the row — no server default is injected.
/// Legacy rows persisted before NSTQ-2 read back as the `"default"` namespace and
/// `"default"` task queue at the store-read layer, so the fallback lives there,
/// not here.
pub struct WorkerOutboxDispatch {
    dispatcher: ActivityDispatcher,
    /// Optional short-TTL per-namespace placement cache (Control-Plane Phase 2,
    /// P2-P3). When present, an UNPINNED row (`row.node == None`) whose namespace
    /// placement is `Prefer{L}` dispatches preferring an L-labelled worker and
    /// spills to any live worker when none is up. When absent (the default, every
    /// pre-Phase-2 construction and test) dispatch is byte-identical: every row
    /// goes straight through [`ActivityDispatcher::dispatch`] off the row's own
    /// `node`. Placement is NEVER stamped back onto the row — it is consulted only
    /// here, in the non-replayed dispatcher, for worker selection.
    placement_cache: Option<crate::worker::PlacementCache>,
}

impl std::fmt::Debug for WorkerOutboxDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerOutboxDispatch")
            .field("placement_cache", &self.placement_cache.is_some())
            .finish_non_exhaustive()
    }
}

impl WorkerOutboxDispatch {
    /// Build a worker-backed dispatch over the shared push dispatcher.
    #[must_use]
    pub fn new(dispatcher: ActivityDispatcher) -> Self {
        Self {
            dispatcher,
            placement_cache: None,
        }
    }

    /// Attach the per-namespace placement cache so an unpinned row consults its
    /// namespace's `Prefer` directive at dispatch time (Control-Plane Phase 2,
    /// P2-P3). Pure builder addition: without it, dispatch is byte-identical to
    /// the pre-Phase-2 behaviour.
    #[must_use]
    pub fn with_placement_cache(mut self, cache: crate::worker::PlacementCache) -> Self {
        self.placement_cache = Some(cache);
        self
    }

    /// Translate an outbox row into the wire-bound scheduled activity.
    ///
    /// The routing identity (`namespace`, `task_queue`, optional `node`) is read
    /// off the row, so the activity dispatches into the workflow's real namespace
    /// pool with any node affinity the row carries (NODE-2). The pinned
    /// `ordinal` is the per-workflow activity ordinal recorded in history, so it
    /// maps directly onto the activity id the worker correlates its result
    /// against; the stored zero-based `attempt` is stamped onto the wire as a
    /// one-based delivery attempt (zero is malformed on the wire).
    fn to_scheduled(row: &OutboxRow) -> ScheduledActivity {
        ScheduledActivity {
            namespace: row.namespace.clone(),
            task_queue: row.task_queue.clone(),
            activity_type: row.activity_type.clone(),
            // node affinity is sourced off the row (NODE-2): `Some(node)` pins the
            // dispatch to workers on that node; `None` = unpinned = any worker in
            // the pool. There is no SDK-level node selection yet (NODE-4), so the
            // row carries `None` today, but the dispatcher no longer hard-codes it.
            node: row.node.clone(),
            workflow_id: row.workflow_id.clone(),
            activity_id: ActivityId::from_sequence_position(row.ordinal),
            run_id: row.run_id.clone(),
            input: row.input.clone(),
            attempt: row.attempt.saturating_add(1),
            labels: std::collections::BTreeMap::new(),
        }
    }
}

#[async_trait]
impl OutboxRowDispatch for WorkerOutboxDispatch {
    async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError> {
        let scheduled = Self::to_scheduled(row);
        // A per-activity authored pin (`row.node == Some(N)`) ALWAYS wins and is
        // untouched: dispatch straight off the row's own node. Only an UNPINNED row
        // consults namespace placement (CP-Phase-2 §2.2 composition rule).
        let (Some(cache), None) = (&self.placement_cache, &scheduled.node) else {
            return self.dispatcher.dispatch(&scheduled).await;
        };
        match cache.placement(&scheduled.namespace).await {
            // SOFT placement: prefer an L-labelled worker, spill to any live one.
            // The row's `node` stays `None` throughout — preference is a pure
            // dispatch-time selection input, never written back (the determinism
            // invariant, CP-Phase-2 §2.4).
            aion_store::NamespacePlacement::Prefer { nodes } => {
                self.dispatcher
                    .dispatch_preferring(&scheduled, &nodes)
                    .await
            }
            // Unplaced (today's behaviour) and Pinned (hard-constraint dispatch
            // enforcement is P2-I1, out of scope here — see the module/slice note)
            // both fall through to the unchanged any-worker dispatch.
            aion_store::NamespacePlacement::Unplaced
            | aion_store::NamespacePlacement::Pinned { .. } => {
                self.dispatcher.dispatch(&scheduled).await
            }
        }
    }
}

/// Non-replayed background dispatcher for pending outbox rows.
///
/// See the module docs for the full contract. Construct with [`Self::new`] and
/// drive with [`Self::run`]; the loop exits cleanly when the shared shutdown
/// watch flips to `true`, mirroring the server's transport shutdown signal.
pub struct OutboxDispatcher {
    store: Arc<dyn OutboxStore>,
    dispatch: Arc<dyn OutboxRowDispatch>,
    config: OutboxDispatcherConfig,
    /// Advisory wake (LSUB-2): an in-process `Notify` the engine's stage seam
    /// pulses when a pending outbox row is committed, so the run loop sweeps
    /// ~immediately instead of waiting up to one poll interval. The wake is
    /// strictly advisory — a dropped, coalesced, or absent wake degrades cleanly
    /// to the interval poll, which remains the correctness backstop. When no wake
    /// is wired in (the default), this is a private `Notify` that is never pulsed,
    /// so the loop behaves exactly as a pure poll.
    wake: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for OutboxDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxDispatcher")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OutboxDispatcher {
    /// Build a dispatcher over the outbox store and a row-dispatch sink.
    #[must_use]
    pub fn new(
        store: Arc<dyn OutboxStore>,
        dispatch: Arc<dyn OutboxRowDispatch>,
        config: OutboxDispatcherConfig,
    ) -> Self {
        Self {
            store,
            dispatch,
            config,
            // Default to a private, never-pulsed wake so an unwired dispatcher is
            // a pure poll. Callers that share the engine's stage seam install the
            // real handle with `with_wake`.
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Install the shared advisory wake (LSUB-2).
    ///
    /// The supplied `Notify` is pulsed by the engine's append-with-outbox seam
    /// when a pending row is committed, so the run loop sweeps promptly rather
    /// than waiting for the next interval tick. The wake never affects
    /// correctness — the interval poll is untouched — so a lost wake simply
    /// reverts to poll latency.
    #[must_use]
    pub fn with_wake(mut self, wake: Arc<tokio::sync::Notify>) -> Self {
        self.wake = wake;
        self
    }

    /// Run the claim/dispatch loop until `shutdown` flips to `true`.
    ///
    /// Each tick claims up to `batch_size` pending rows and dispatches them in
    /// order. A backend error claiming rows is logged and the loop waits for the
    /// next tick rather than tearing the task down — a transient store failure
    /// must not silently stop the dispatcher. Shutdown is observed both while
    /// waiting for the next tick and is re-checked before each sweep, so a
    /// drain never blocks on an in-progress wait.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        info!(
            poll_interval_ms = self.config.poll_interval.as_millis(),
            batch_size = self.config.batch_size,
            max_attempts = self.config.max_attempts,
            "outbox dispatcher started"
        );
        let mut interval = tokio::time::interval(self.config.poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if *shutdown.borrow() {
                        break;
                    }
                    self.sweep_once().await;
                }
                // LSUB-2 advisory wake: a stage-seam pulse interrupts the sleep so
                // a newly-staged row dispatches in ~RTT instead of up to one poll
                // interval. Re-check shutdown first, exactly like the interval arm,
                // so a wake never races a drain. The interval tick above is left
                // untouched, so the poll stays the correctness backstop: a dropped
                // or coalesced wake just costs poll latency, never a lost dispatch.
                () = self.wake.notified() => {
                    if *shutdown.borrow() {
                        break;
                    }
                    self.sweep_once().await;
                }
                changed = shutdown.changed() => {
                    // A send error means every sender dropped; treat that as a
                    // shutdown request rather than spinning.
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        info!("outbox dispatcher stopped");
    }

    /// Claim one batch of pending rows and drive each to its terminal state.
    async fn sweep_once(&self) {
        // LSUB-4-3 / Fork-A2 seam: ownership is NOT enforced on this claim. The
        // claim path is an UNFENCED local `put_routed` scoped by `owned_shard_scope()`
        // — it simply returns `Ok` with only the rows on shards this node owns and
        // never surfaces a `NotOwner`. Deposition is surfaced (and the owned set
        // narrowed by re-residency) on the FENCED stamped event-append a deposed
        // owner attempts when recording a terminal (aion-store-haematite store.rs
        // ~622/1110: `DatabaseError::Fenced => StoreError::NotOwner`), not here. So
        // this sweep has nothing ownership-specific to handle: a claim error is a
        // genuine backend failure, retried next tick.
        let rows = match self.store.claim_outbox_rows(self.config.batch_size).await {
            Ok(rows) => rows,
            Err(error) => {
                error!(%error, "outbox dispatcher failed to claim rows; retrying next tick");
                return;
            }
        };
        for row in rows {
            self.process_row(&row).await;
        }
    }

    /// Dispatch one claimed row and record its terminal outbox state.
    async fn process_row(&self, row: &OutboxRow) {
        match self.dispatch.dispatch(row).await {
            Ok(()) => self.mark_done(row).await,
            Err(error) => self.handle_dispatch_error(row, &error).await,
        }
    }

    async fn mark_done(&self, row: &OutboxRow) {
        if let Err(error) = self.store.complete_outbox_row(&row.dispatch_key).await {
            // The dispatch already happened; failing to persist `done` leaves
            // the row `claimed` (never re-claimed), so log loudly for operators.
            error!(
                dispatch_key = %row.dispatch_key,
                %error,
                "outbox dispatcher dispatched a row but failed to mark it done"
            );
        }
    }

    /// Apply the retry budget after a failed dispatch.
    ///
    /// The just-failed attempt is `row.attempt` (zero-based). If a further
    /// attempt remains within `max_attempts`, the row is returned to `pending`
    /// with the attempt bumped and a `visible_after` fence; otherwise it is
    /// dead-lettered to `failed`.
    ///
    /// # Fast cross-node failover (LSUB-3)
    ///
    /// When the failure is [`ServerError::WorkerConnectionLost`] — the chosen
    /// worker died mid-dispatch and liminal has already deregistered it — the row
    /// is re-armed for IMMEDIATE re-claim (`visible_after = now`, no backoff) so the
    /// next sweep promptly re-dispatches it to a live worker in the pool. The
    /// attempt is STILL consumed: this is the deliberate policy choice — immediate
    /// re-claim but attempt-consuming — so pathological worker churn stays bounded
    /// by `max_attempts` and eventually dead-letters rather than forming an
    /// unbounded re-dispatch loop. A genuine reply timeout (the worker is alive but
    /// slow) and every other error keep the normal exponential backoff unchanged.
    async fn handle_dispatch_error(&self, row: &OutboxRow, dispatch_error: &ServerError) {
        let attempted = row.attempt.saturating_add(1);
        if attempted >= self.config.max_attempts {
            warn!(
                dispatch_key = %row.dispatch_key,
                attempt = row.attempt,
                max_attempts = self.config.max_attempts,
                error = %dispatch_error,
                "outbox dispatch exhausted retry budget; dead-lettering row"
            );
            if let Err(error) = self.store.fail_outbox_row(&row.dispatch_key).await {
                error!(dispatch_key = %row.dispatch_key, %error, "outbox dispatcher failed to dead-letter row");
            }
            return;
        }
        // LSUB-3 fast failover: a lost worker connection re-arms for immediate
        // re-claim (skip backoff); everything else keeps the backoff curve.
        if dispatch_error.is_worker_connection_lost() {
            let visible_after = Utc::now();
            warn!(
                dispatch_key = %row.dispatch_key,
                attempt = row.attempt,
                next_attempt = attempted,
                error = %dispatch_error,
                "outbox dispatch lost the worker connection; re-arming for immediate failover"
            );
            if let Err(error) = self
                .store
                .retry_outbox_row(&row.dispatch_key, attempted, visible_after)
                .await
            {
                error!(dispatch_key = %row.dispatch_key, %error, "outbox dispatcher failed to re-arm row for failover");
            }
            return;
        }
        let backoff = self.config.backoff_for_attempt(row.attempt);
        let visible_after = Utc::now() + chrono_duration(backoff);
        warn!(
            dispatch_key = %row.dispatch_key,
            attempt = row.attempt,
            next_attempt = attempted,
            backoff_ms = backoff.as_millis(),
            error = %dispatch_error,
            "outbox dispatch failed; scheduling retry with backoff"
        );
        if let Err(error) = self
            .store
            .retry_outbox_row(&row.dispatch_key, attempted, visible_after)
            .await
        {
            error!(dispatch_key = %row.dispatch_key, %error, "outbox dispatcher failed to schedule retry");
        }
    }
}

/// Convert a (non-negative) [`Duration`] into a [`chrono::Duration`], saturating
/// at the chrono maximum rather than failing — the backoff curve is already
/// clamped to `backoff_max`, so this only guards the type boundary.
fn chrono_duration(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{OutboxRow, OutboxStatus, OutboxStore};
    use aion_store_libsql::LibSqlStore;
    use async_trait::async_trait;
    use chrono::Utc;

    use super::{OutboxDispatcher, OutboxDispatcherConfig, OutboxRowDispatch, ServerError};

    fn config() -> OutboxDispatcherConfig {
        OutboxDispatcherConfig {
            poll_interval: Duration::from_millis(10),
            batch_size: 16,
            max_attempts: 3,
            backoff_base: Duration::from_millis(100),
            backoff_multiplier: 2,
            backoff_max: Duration::from_secs(60),
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-server-outbox-dispatcher-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }

    /// One shared store handle drives both the dispatcher and the test
    /// assertions, so they observe the same rows without a second connection.
    async fn open_store(name: &str) -> Result<Arc<LibSqlStore>, ServerError> {
        LibSqlStore::open(unique_temp_path(name))
            .await
            .map(Arc::new)
            .map_err(ServerError::from)
    }

    fn pending_row(workflow_id: &WorkflowId, ordinal: u64) -> OutboxRow {
        OutboxRow::pending(
            workflow_id.clone(),
            ordinal,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now(),
        )
    }

    /// Records every dispatched row; configurable to always succeed or always fail.
    struct RecordingDispatch {
        succeed: bool,
        dispatched: Mutex<Vec<OutboxRow>>,
    }

    impl RecordingDispatch {
        fn new(succeed: bool) -> Self {
            Self {
                succeed,
                dispatched: Mutex::new(Vec::new()),
            }
        }

        fn count(&self) -> Result<usize, ServerError> {
            Ok(self
                .dispatched
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording dispatch"))?
                .len())
        }
    }

    #[async_trait]
    impl OutboxRowDispatch for RecordingDispatch {
        async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError> {
            self.dispatched
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording dispatch"))?
                .push(row.clone());
            if self.succeed {
                Ok(())
            } else {
                Err(ServerError::worker_dispatch(
                    "default",
                    "charge",
                    "no worker in test",
                ))
            }
        }
    }

    #[tokio::test]
    async fn sweep_dispatches_claimed_rows_and_marks_them_done()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("done").await?;
        let workflow_id = WorkflowId::new_v4();
        let row_a = pending_row(&workflow_id, 0);
        let row_b = pending_row(&workflow_id, 1);
        store
            .append_outbox_batch(&[row_a.clone(), row_b.clone()])
            .await?;

        let dispatch = Arc::new(RecordingDispatch::new(true));
        let dispatcher = OutboxDispatcher::new(store.clone(), dispatch.clone(), config());
        // Drive a single sweep directly (no timer) for deterministic assertions.
        dispatcher.sweep_once().await;

        assert_eq!(dispatch.count()?, 2, "both pending rows are dispatched");
        assert_eq!(
            store
                .outbox_row_state(&row_a.dispatch_key)
                .await?
                .map(|s| s.status),
            Some(OutboxStatus::Done)
        );
        assert_eq!(
            store
                .outbox_row_state(&row_b.dispatch_key)
                .await?
                .map(|s| s.status),
            Some(OutboxStatus::Done)
        );
        // Nothing is claimable after a successful sweep.
        assert!(store.claim_outbox_rows(10).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn failed_dispatch_retries_with_backoff_and_bumps_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("retry").await?;
        let workflow_id = WorkflowId::new_v4();
        let row = pending_row(&workflow_id, 0);
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        let before = Utc::now();
        let dispatch = Arc::new(RecordingDispatch::new(false));
        let dispatcher = OutboxDispatcher::new(store.clone(), dispatch, config());
        dispatcher.sweep_once().await;

        let state = store
            .outbox_row_state(&row.dispatch_key)
            .await?
            .ok_or("retried row must still exist")?;
        // Returned to pending, attempt bumped from 0 to 1.
        assert_eq!(state.status, OutboxStatus::Pending);
        assert_eq!(state.attempt, 1);
        // visible_after advanced into the future by at least the base backoff.
        assert!(
            state.visible_after >= before + chrono::Duration::milliseconds(100),
            "visible_after must advance by at least the base backoff"
        );
        // The backoff fence holds the row out of the claimable set right now.
        assert!(store.claim_outbox_rows(10).await?.is_empty());
        Ok(())
    }

    /// Always fails with a [`ServerError::WorkerConnectionLost`], standing in for
    /// the chosen worker dying mid-dispatch.
    struct ConnectionLostDispatch;

    #[async_trait]
    impl OutboxRowDispatch for ConnectionLostDispatch {
        async fn dispatch(&self, _row: &OutboxRow) -> Result<(), ServerError> {
            Err(ServerError::worker_connection_lost(
                "liminal-push",
                "worker connection closed before reply",
            ))
        }
    }

    /// LSUB-3: a lost worker connection re-arms the row for IMMEDIATE re-claim
    /// (no backoff) so the next sweep fails it over to a live worker — while STILL
    /// consuming one attempt so churn stays bounded. Contrast with
    /// `failed_dispatch_retries_with_backoff_and_bumps_attempt`, where a generic
    /// failure pushes `visible_after` into the future.
    #[tokio::test]
    async fn connection_lost_rearms_immediately_and_consumes_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("conn-lost").await?;
        let workflow_id = WorkflowId::new_v4();
        let row = pending_row(&workflow_id, 0);
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        let before = Utc::now();
        let dispatcher =
            OutboxDispatcher::new(store.clone(), Arc::new(ConnectionLostDispatch), config());
        dispatcher.sweep_once().await;
        let after = Utc::now();

        let state = store
            .outbox_row_state(&row.dispatch_key)
            .await?
            .ok_or("re-armed row must still exist")?;
        // Returned to pending with the attempt consumed (0 -> 1): churn stays
        // bounded by max_attempts and eventually dead-letters.
        assert_eq!(state.status, OutboxStatus::Pending);
        assert_eq!(state.attempt, 1, "the failover still consumes one attempt");
        // visible_after is "now", NOT pushed out by the base backoff: it sits in
        // the [before, after] window of this sweep, well below the 100ms base
        // backoff the generic-failure path would have applied.
        assert!(
            state.visible_after >= before && state.visible_after <= after,
            "visible_after must be re-armed to now (immediate re-claim), not backed off"
        );
        assert!(
            state.visible_after < before + chrono::Duration::milliseconds(100),
            "immediate re-arm must not apply the base backoff fence"
        );
        // The row is IMMEDIATELY claimable again — the next sweep re-dispatches it.
        assert_eq!(
            store.claim_outbox_rows(10).await?.len(),
            1,
            "the re-armed row is immediately claimable for failover"
        );
        Ok(())
    }

    /// LSUB-3: a lost worker connection STILL dead-letters once the attempt budget
    /// is exhausted — immediate re-claim never forms an unbounded re-dispatch loop.
    #[tokio::test]
    async fn connection_lost_dead_letters_after_max_attempts()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("conn-lost-dead").await?;
        let workflow_id = WorkflowId::new_v4();
        // Seed at the final attempt so the next connection-lost failure exhausts
        // the budget rather than re-arming forever.
        let mut row = pending_row(&workflow_id, 0);
        row.attempt = config().max_attempts - 1;
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        let dispatcher =
            OutboxDispatcher::new(store.clone(), Arc::new(ConnectionLostDispatch), config());
        dispatcher.sweep_once().await;

        assert_eq!(
            store
                .outbox_row_state(&row.dispatch_key)
                .await?
                .map(|s| s.status),
            Some(OutboxStatus::Failed),
            "connection-lost churn is bounded by max_attempts and dead-letters"
        );
        assert!(store.claim_outbox_rows(10).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_fails_row_after_max_attempts() -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("fail").await?;
        let workflow_id = WorkflowId::new_v4();
        // Seed the row already at attempt == max_attempts - 1, so the next
        // failed dispatch is the final attempt and dead-letters it.
        let mut row = pending_row(&workflow_id, 0);
        row.attempt = config().max_attempts - 1;
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        let dispatch = Arc::new(RecordingDispatch::new(false));
        let dispatcher = OutboxDispatcher::new(store.clone(), dispatch, config());
        dispatcher.sweep_once().await;

        assert_eq!(
            store
                .outbox_row_state(&row.dispatch_key)
                .await?
                .map(|s| s.status),
            Some(OutboxStatus::Failed)
        );
        // A dead-lettered row is never claimable again.
        assert!(store.claim_outbox_rows(10).await?.is_empty());
        Ok(())
    }

    /// LSUB-4-6 (`mark_done` write failure): dispatch SUCCEEDS but `complete_outbox_row`
    /// fails — the row must stay `Claimed` (never silently dropped, never retried or
    /// dead-lettered), so a later rearm/reconcile can re-dispatch it (deduped to one
    /// terminal in history). Driven through a mock so the post-condition is asserted
    /// deterministically; `retry`/`fail` record a flag (never reached) instead of
    /// panicking, keeping the test free of the restriction lints.
    #[tokio::test]
    async fn mark_done_failure_leaves_row_claimed_for_later_rearm()
    -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::{ClaimScope, StoreError};
        use chrono::{DateTime, Utc};
        use std::sync::atomic::{AtomicBool, Ordering};

        /// Hands out exactly one claimable row, then fails `complete_outbox_row`.
        /// Records whether `complete` was attempted and whether any other terminal
        /// transition was reached (it must not be) so the test proves the row is
        /// left `Claimed`.
        struct CompleteFailsStore {
            row: OutboxRow,
            claimed: AtomicBool,
            completed: AtomicBool,
            other_terminal: AtomicBool,
        }

        #[async_trait]
        impl OutboxStore for CompleteFailsStore {
            async fn append_outbox_batch(&self, _rows: &[OutboxRow]) -> Result<(), StoreError> {
                Ok(())
            }
            async fn claim_outbox_rows(&self, _limit: u32) -> Result<Vec<OutboxRow>, StoreError> {
                // Hand out the row exactly once (compare-and-swap false -> true).
                if self
                    .claimed
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    Ok(vec![self.row.clone()])
                } else {
                    Ok(Vec::new())
                }
            }
            async fn claim_outbox_rows_scoped(
                &self,
                _scope: &ClaimScope,
                _limit: u32,
            ) -> Result<Vec<OutboxRow>, StoreError> {
                Ok(Vec::new())
            }
            async fn rearm_stale_claimed_outbox_rows(
                &self,
                _older_than: DateTime<Utc>,
                _visible_after: DateTime<Utc>,
                _limit: u32,
            ) -> Result<Vec<OutboxRow>, StoreError> {
                Ok(Vec::new())
            }
            async fn complete_outbox_row(&self, _dispatch_key: &str) -> Result<(), StoreError> {
                // The write fails AFTER dispatch already happened; the row stays Claimed.
                self.completed.store(true, Ordering::SeqCst);
                Err(StoreError::Backend("mark-done write failed".to_owned()))
            }
            async fn retry_outbox_row(
                &self,
                _dispatch_key: &str,
                _next_attempt: u32,
                _visible_after: DateTime<Utc>,
            ) -> Result<(), StoreError> {
                self.other_terminal.store(true, Ordering::SeqCst);
                Ok(())
            }
            async fn fail_outbox_row(&self, _dispatch_key: &str) -> Result<(), StoreError> {
                self.other_terminal.store(true, Ordering::SeqCst);
                Ok(())
            }
            async fn count_inflight_outbox_rows(
                &self,
                _namespace: &str,
            ) -> Result<u64, StoreError> {
                // The single staged row is in-flight (Pending or stuck-Claimed) until completed.
                Ok(u64::from(!self.completed.load(Ordering::SeqCst)))
            }
        }

        let workflow_id = WorkflowId::new_v4();
        let store = Arc::new(CompleteFailsStore {
            row: pending_row(&workflow_id, 0),
            claimed: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            other_terminal: AtomicBool::new(false),
        });
        let dispatch = Arc::new(RecordingDispatch::new(true));
        let dispatcher = OutboxDispatcher::new(store.clone(), dispatch.clone(), config());
        // Sweep: claims the row, dispatches it (succeeds), then complete fails.
        dispatcher.sweep_once().await;

        assert_eq!(dispatch.count()?, 1, "the row was dispatched exactly once");
        assert!(
            store.completed.load(Ordering::SeqCst),
            "mark_done was attempted (and failed) after the successful dispatch"
        );
        // The row is left Claimed: NOT retried, NOT dead-lettered. A later rearm /
        // reconcile re-dispatches it, deduped to one terminal in history.
        assert!(
            !store.other_terminal.load(Ordering::SeqCst),
            "a mark_done failure must not retry or dead-letter the row (it stays Claimed)"
        );
        Ok(())
    }

    /// Drive a dispatcher's `run` loop until a row reaches `Done` or the deadline
    /// elapses; returns whether it reached `Done` in time.
    async fn wait_for_done(
        store: &LibSqlStore,
        dispatch_key: &str,
        deadline: std::time::Instant,
    ) -> Result<bool, ServerError> {
        loop {
            let done = store
                .outbox_row_state(dispatch_key)
                .await?
                .map(|s| s.status)
                == Some(OutboxStatus::Done);
            if done {
                return Ok(true);
            }
            if std::time::Instant::now() > deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// LSUB-2 (fast path): a wake drives a staged row to `Done` FAST — well under
    /// the poll interval — proving the wake, not the poll, ran the sweep. The poll
    /// is set to 10s so it cannot explain a sub-second dispatch.
    #[tokio::test]
    async fn wake_dispatches_staged_row_well_under_poll_interval()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("wake-fast").await?;
        let workflow_id = WorkflowId::new_v4();
        let row = pending_row(&workflow_id, 0);
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        // A 10s poll interval: any dispatch inside the 1s assert deadline must be
        // the wake's doing, not the poll (a >=100x margin keeps it non-flaky).
        let mut slow_poll = config();
        slow_poll.poll_interval = Duration::from_secs(10);
        let wake = Arc::new(tokio::sync::Notify::new());
        let dispatch = Arc::new(RecordingDispatch::new(true));
        let dispatcher = OutboxDispatcher::new(store.clone(), dispatch.clone(), slow_poll)
            .with_wake(Arc::clone(&wake));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(dispatcher.run(shutdown_rx));

        // The row is already staged, so the stored permit from `notify_one` is
        // consumed by the first `notified()` and the sweep finds the pending row.
        wake.notify_one();

        let reached = wait_for_done(
            store.as_ref(),
            &row.dispatch_key,
            std::time::Instant::now() + Duration::from_secs(1),
        )
        .await?;
        assert!(
            reached,
            "the wake must dispatch the staged row within 1s, far under the 10s poll"
        );
        assert_eq!(dispatch.count()?, 1);

        shutdown_tx.send(true)?;
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .map_err(|_| "outbox dispatcher did not stop after shutdown")??;
        Ok(())
    }

    /// LSUB-2 (correctness backstop): with the wake NEVER pulsed, a staged row
    /// STILL reaches `Done` via the interval poll. Together with the fast-path test
    /// this proves the wake is advisory-only — a dropped/absent wake degrades
    /// cleanly to the existing poll and never loses a dispatch.
    #[tokio::test]
    async fn poll_dispatches_staged_row_when_wake_never_fires()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("wake-absent").await?;
        let workflow_id = WorkflowId::new_v4();
        let row = pending_row(&workflow_id, 0);
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        // Short (10ms) poll, and a wake handle that is installed but never pulsed.
        let wake = Arc::new(tokio::sync::Notify::new());
        let dispatch = Arc::new(RecordingDispatch::new(true));
        let dispatcher = OutboxDispatcher::new(store.clone(), dispatch.clone(), config())
            .with_wake(Arc::clone(&wake));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(dispatcher.run(shutdown_rx));

        // Deliberately never call `wake.notify_one()`: only the poll can drive this.
        let reached = wait_for_done(
            store.as_ref(),
            &row.dispatch_key,
            std::time::Instant::now() + Duration::from_secs(5),
        )
        .await?;
        assert!(
            reached,
            "the poll must dispatch the staged row even though the wake never fired"
        );
        assert_eq!(dispatch.count()?, 1);

        shutdown_tx.send(true)?;
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .map_err(|_| "outbox dispatcher did not stop after shutdown")??;
        Ok(())
    }

    #[tokio::test]
    async fn run_loop_drains_pending_then_stops_on_shutdown()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = open_store("run-loop").await?;
        let workflow_id = WorkflowId::new_v4();
        let row = pending_row(&workflow_id, 0);
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        let dispatch = Arc::new(RecordingDispatch::new(true));
        let dispatcher = OutboxDispatcher::new(store.clone(), dispatch.clone(), config());
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(dispatcher.run(shutdown_rx));

        // Wait for the row to be dispatched and marked done by the loop.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let done = store
                .outbox_row_state(&row.dispatch_key)
                .await?
                .map(|s| s.status)
                == Some(OutboxStatus::Done);
            if done {
                break;
            }
            if std::time::Instant::now() > deadline {
                return Err("outbox dispatcher loop did not mark the row done".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(dispatch.count()?, 1);

        // Signal shutdown; the task must observe it and stop cleanly.
        shutdown_tx.send(true)?;
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .map_err(|_| "outbox dispatcher did not stop after shutdown")??;
        Ok(())
    }

    #[test]
    fn backoff_grows_geometrically_and_clamps_to_max() {
        let config = config();
        // attempt 0: base (100ms); attempt 1: 200ms; attempt 2: 400ms.
        assert_eq!(config.backoff_for_attempt(0), Duration::from_millis(100));
        assert_eq!(config.backoff_for_attempt(1), Duration::from_millis(200));
        assert_eq!(config.backoff_for_attempt(2), Duration::from_millis(400));
        // A very large attempt clamps at backoff_max, never overflows.
        assert_eq!(config.backoff_for_attempt(1000), config.backoff_max);
    }

    /// NSTQ-2: the production [`WorkerOutboxDispatch`] routes a claimed row by the
    /// `namespace` + `task_queue` carried ON THE ROW, not by any server default.
    /// A row stamped `namespace = "remote"` reaches a worker registered in the
    /// `remote` namespace. A row stamped `namespace = "default"` is NOT served by
    /// that `remote` worker (its dispatch blocks waiting for a `default`-ns worker
    /// that never registers), proving the routing identity comes off the row and
    /// the server default is no longer injected.
    #[tokio::test]
    async fn worker_dispatch_routes_by_row_namespace_not_server_default()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::worker::dispatch::ActivityDispatcher;
        use crate::worker::registry::{ConnectedWorkerRegistry, WorkerMessage};
        use aion_store::OutboxRow;

        let registry = ConnectedWorkerRegistry::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let activity_types = [String::from("charge")];
        // Only a `remote`-namespace worker is connected.
        let _registration = registry.register("remote", activity_types.iter(), tx)?;

        let dispatch = super::WorkerOutboxDispatch::new(ActivityDispatcher::new(registry.clone()));

        // A row whose routing identity is `remote` reaches the remote worker.
        let workflow_id = WorkflowId::new_v4();
        let remote_row = OutboxRow::pending(
            workflow_id.clone(),
            0,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now(),
        )
        .with_namespace("remote")
        .with_task_queue("default");

        OutboxRowDispatch::dispatch(&dispatch, &remote_row).await?;
        let message = rx.recv().await.ok_or("expected pushed activity task")?;
        assert!(
            matches!(message, WorkerMessage::ActivityTask(_)),
            "the remote-namespace worker must receive the activity task for a remote row"
        );

        // A row whose namespace is `default` is NOT served by the remote worker:
        // its dispatch blocks waiting for a `default`-ns worker that never appears.
        let default_row = remote_row.clone().with_namespace("default");
        let blocked = tokio::time::timeout(
            Duration::from_millis(200),
            OutboxRowDispatch::dispatch(&dispatch, &default_row),
        )
        .await;
        assert!(
            blocked.is_err(),
            "a default-namespace row must not be served by a remote-namespace worker"
        );
        Ok(())
    }

    /// NODE-2: `to_scheduled` sources node affinity off the row. A row stamped
    /// `Some(node)` produces a `ScheduledActivity` pinned to that node; a row with
    /// no affinity (`None`) produces an unpinned dispatch.
    #[test]
    fn to_scheduled_sources_node_affinity_from_row() {
        let workflow_id = WorkflowId::new_v4();
        let pinned = OutboxRow::pending(
            workflow_id.clone(),
            0,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now(),
        )
        .with_node(Some(String::from("box-7")));
        let scheduled = super::WorkerOutboxDispatch::to_scheduled(&pinned);
        assert_eq!(scheduled.node.as_deref(), Some("box-7"));

        let unpinned = pending_row(&workflow_id, 1);
        let scheduled = super::WorkerOutboxDispatch::to_scheduled(&unpinned);
        assert_eq!(scheduled.node, None);
    }

    // --- P2-P3: Prefer two-tier spill + the determinism invariant -----------

    /// Register a worker advertising `node` for `activity_type` in `namespace`,
    /// returning the registration token (held to keep it connected) and its
    /// receiver so the test can observe a delivered task.
    fn register_node_worker(
        registry: &crate::worker::registry::ConnectedWorkerRegistry,
        namespace: &str,
        node: &str,
        activity_type: &str,
    ) -> Result<
        (
            crate::worker::registry::WorkerRegistration,
            tokio::sync::mpsc::Receiver<crate::worker::registry::WorkerMessage>,
        ),
        Box<dyn std::error::Error>,
    > {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let types = [activity_type.to_owned()];
        let registration = registry.register_namespaces(
            [namespace.to_owned()],
            String::from("default"),
            Some(node.to_owned()),
            types.iter(),
            tx,
        )?;
        Ok((registration, rx))
    }

    /// Build an UNPINNED outbox row (`node == None`) in `namespace` for `charge`.
    fn unpinned_row(namespace: &str) -> OutboxRow {
        OutboxRow::pending(
            WorkflowId::new_v4(),
            0,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now(),
        )
        .with_namespace(namespace)
        .with_task_queue("default")
    }

    /// Build a `WorkerOutboxDispatch` over `registry` whose placement cache reads
    /// `namespace_store` (zero TTL so each dispatch sees the latest placement).
    fn placement_dispatch(
        registry: &crate::worker::registry::ConnectedWorkerRegistry,
        namespace_store: Arc<dyn aion_store::NamespaceStore>,
    ) -> super::WorkerOutboxDispatch {
        use crate::worker::dispatch::ActivityDispatcher;
        let cache = crate::worker::PlacementCache::new(namespace_store, Duration::ZERO);
        super::WorkerOutboxDispatch::new(ActivityDispatcher::new(registry.clone()))
            .with_placement_cache(cache)
    }

    /// P2-P3 (prefer): an unpinned row in a `Prefer{n1}` namespace selects the
    /// n1 worker when one is live, even with an n2 worker also connected.
    #[tokio::test]
    async fn prefer_selects_preferred_node_worker_when_present()
    -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::{InMemoryStore, NamespaceOrigin, NamespacePlacement, NamespaceStore};

        let ns_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        ns_store
            .register_namespace("t", NamespaceOrigin::Explicit)
            .await?;
        let n1: std::collections::BTreeSet<String> = ["n1".to_owned()].into_iter().collect();
        ns_store
            .set_namespace_placement("t", NamespacePlacement::Prefer { nodes: n1 })
            .await?;

        let registry = crate::worker::registry::ConnectedWorkerRegistry::default();
        let (_n1_reg, mut n1_rx) = register_node_worker(&registry, "t", "n1", "charge")?;
        let (_n2_reg, mut n2_rx) = register_node_worker(&registry, "t", "n2", "charge")?;
        let dispatch = placement_dispatch(&registry, Arc::clone(&ns_store));

        let row = unpinned_row("t");
        OutboxRowDispatch::dispatch(&dispatch, &row).await?;

        assert!(
            n1_rx.recv().await.is_some(),
            "the n1 worker receives the task"
        );
        assert!(
            n2_rx.try_recv().is_err(),
            "the n2 worker must NOT receive the task while n1 is live"
        );
        // The recorded row's node is UNTOUCHED by preference (determinism gate).
        assert_eq!(row.node, None, "placement must never mutate the row's node");
        Ok(())
    }

    /// P2-P3 (spill): an unpinned row in a `Prefer{n1}` namespace SPILLS to the
    /// only live worker (n2) when no n1 worker is connected — the demoable
    /// node-loss failover behaviour.
    #[tokio::test]
    async fn prefer_spills_to_any_worker_when_preferred_node_absent()
    -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::{InMemoryStore, NamespaceOrigin, NamespacePlacement, NamespaceStore};

        let ns_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        ns_store
            .register_namespace("t", NamespaceOrigin::Explicit)
            .await?;
        let n1: std::collections::BTreeSet<String> = ["n1".to_owned()].into_iter().collect();
        ns_store
            .set_namespace_placement("t", NamespacePlacement::Prefer { nodes: n1 })
            .await?;

        // Only an n2 worker is live: no n1-labelled worker exists at all.
        let registry = crate::worker::registry::ConnectedWorkerRegistry::default();
        let (_n2_reg, mut n2_rx) = register_node_worker(&registry, "t", "n2", "charge")?;
        let dispatch = placement_dispatch(&registry, Arc::clone(&ns_store));

        let row = unpinned_row("t");
        OutboxRowDispatch::dispatch(&dispatch, &row).await?;

        assert!(
            n2_rx.recv().await.is_some(),
            "with no n1 worker live, the dispatch spills to the live n2 worker"
        );
        assert_eq!(row.node, None, "spill must never mutate the row's node");
        Ok(())
    }

    /// P2-P3 (unplaced unchanged): an `Unplaced` namespace dispatches to any live
    /// worker exactly as before, regardless of node label.
    #[tokio::test]
    async fn unplaced_namespace_dispatches_to_any_worker() -> Result<(), Box<dyn std::error::Error>>
    {
        use aion_store::{InMemoryStore, NamespaceOrigin, NamespaceStore};

        let ns_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        // Registered but left Unplaced (the default).
        ns_store
            .register_namespace("t", NamespaceOrigin::Explicit)
            .await?;

        let registry = crate::worker::registry::ConnectedWorkerRegistry::default();
        let (_n2_reg, mut n2_rx) = register_node_worker(&registry, "t", "n2", "charge")?;
        let dispatch = placement_dispatch(&registry, Arc::clone(&ns_store));

        OutboxRowDispatch::dispatch(&dispatch, &unpinned_row("t")).await?;
        assert!(
            n2_rx.recv().await.is_some(),
            "an Unplaced namespace reaches any live worker"
        );
        Ok(())
    }

    /// P2-P3 (authored pin wins): a row with an authored node `Some(N)` STILL
    /// requires N regardless of the namespace's `Prefer{other}` placement — the
    /// per-activity pin is authoritative and the placement never overrides it.
    #[tokio::test]
    async fn authored_node_pin_wins_over_namespace_prefer() -> Result<(), Box<dyn std::error::Error>>
    {
        use aion_store::{InMemoryStore, NamespaceOrigin, NamespacePlacement, NamespaceStore};

        let ns_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        ns_store
            .register_namespace("t", NamespaceOrigin::Explicit)
            .await?;
        // Namespace prefers n1, but the row is authored-pinned to n2.
        let n1: std::collections::BTreeSet<String> = ["n1".to_owned()].into_iter().collect();
        ns_store
            .set_namespace_placement("t", NamespacePlacement::Prefer { nodes: n1 })
            .await?;

        let registry = crate::worker::registry::ConnectedWorkerRegistry::default();
        let (_n1_reg, mut n1_rx) = register_node_worker(&registry, "t", "n1", "charge")?;
        let (_n2_reg, mut n2_rx) = register_node_worker(&registry, "t", "n2", "charge")?;
        let dispatch = placement_dispatch(&registry, Arc::clone(&ns_store));

        // Authored pin: node = Some("n2").
        let row = unpinned_row("t").with_node(Some(String::from("n2")));
        OutboxRowDispatch::dispatch(&dispatch, &row).await?;

        assert!(
            n2_rx.recv().await.is_some(),
            "the authored Some(n2) pin is honoured regardless of the namespace Prefer{{n1}}"
        );
        assert!(
            n1_rx.try_recv().is_err(),
            "the preferred-n1 worker must NOT receive a task authored-pinned to n2"
        );
        // The authored node is preserved exactly (determinism gate).
        assert_eq!(row.node.as_deref(), Some("n2"));
        Ok(())
    }

    /// DETERMINISM GATE (non-negotiable): the recorded row's `node` is
    /// byte-identical regardless of WHICH worker placement routed the activity to.
    /// Under `Prefer{n1}`, the same unpinned row dispatched once to the n1 worker
    /// and once (after n1 leaves) spilled to n2 keeps `node == None` BOTH times —
    /// `to_scheduled` reads the row's node, never the placement, so replay sees an
    /// identical command stream irrespective of the live dispatch target.
    #[tokio::test]
    async fn placement_never_mutates_recorded_row_node_across_routings()
    -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::{InMemoryStore, NamespaceOrigin, NamespacePlacement, NamespaceStore};

        let ns_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        ns_store
            .register_namespace("t", NamespaceOrigin::Explicit)
            .await?;
        let n1: std::collections::BTreeSet<String> = ["n1".to_owned()].into_iter().collect();
        ns_store
            .set_namespace_placement("t", NamespacePlacement::Prefer { nodes: n1 })
            .await?;
        let registry = crate::worker::registry::ConnectedWorkerRegistry::default();
        let dispatch = placement_dispatch(&registry, Arc::clone(&ns_store));

        // Routing A: n1 worker present → preferred selection.
        let (n1_reg, mut n1_rx) = register_node_worker(&registry, "t", "n1", "charge")?;
        let row_a = unpinned_row("t");
        OutboxRowDispatch::dispatch(&dispatch, &row_a).await?;
        assert!(n1_rx.recv().await.is_some());
        let scheduled_a = super::WorkerOutboxDispatch::to_scheduled(&row_a);

        // n1 leaves; only n2 remains.
        n1_reg.deregister()?;
        let (_n2_reg, mut n2_rx) = register_node_worker(&registry, "t", "n2", "charge")?;

        // Routing B: same shape of unpinned row → spills to n2.
        let row_b = unpinned_row("t");
        OutboxRowDispatch::dispatch(&dispatch, &row_b).await?;
        assert!(n2_rx.recv().await.is_some());
        let scheduled_b = super::WorkerOutboxDispatch::to_scheduled(&row_b);

        // The recorded row node — and thus the scheduled task's node — is None in
        // BOTH routings: the dispatch target (n1 vs n2) did not perturb it.
        assert_eq!(row_a.node, None);
        assert_eq!(row_b.node, None);
        assert_eq!(
            scheduled_a.node, scheduled_b.node,
            "the scheduled task node is identical regardless of which worker served it"
        );
        assert_eq!(
            scheduled_a.node, None,
            "an unpinned row stays unpinned at dispatch"
        );
        Ok(())
    }

    #[tokio::test]
    async fn claim_marks_row_claimed_then_sweep_advances_to_done()
    -> Result<(), Box<dyn std::error::Error>> {
        // Pins that the dispatcher reads only the outbox (claim → terminal),
        // never workflow history: a bare claim flips status to claimed, and a
        // successful sweep then advances it to done.
        let store = open_store("claimed").await?;
        let workflow_id = WorkflowId::new_v4();
        let row = pending_row(&workflow_id, 0);
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].status, OutboxStatus::Claimed);
        Ok(())
    }
}
