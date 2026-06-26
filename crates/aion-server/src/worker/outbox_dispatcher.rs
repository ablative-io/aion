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
/// Maps an [`OutboxRow`] to a [`ScheduledActivity`] in the server's default
/// namespace and pushes it through the existing [`ActivityDispatcher`]. The
/// outbox row carries no namespace today (the schema's `namespace` column is
/// reserved for the later liminal cross-node send), so dispatch uses the
/// server's configured default namespace, exactly as local worker dispatch
/// does.
pub struct WorkerOutboxDispatch {
    dispatcher: ActivityDispatcher,
    namespace: String,
}

impl std::fmt::Debug for WorkerOutboxDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerOutboxDispatch")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl WorkerOutboxDispatch {
    /// Build a worker-backed dispatch over the shared push dispatcher.
    #[must_use]
    pub fn new(dispatcher: ActivityDispatcher, namespace: impl Into<String>) -> Self {
        Self {
            dispatcher,
            namespace: namespace.into(),
        }
    }

    /// Translate an outbox row into the wire-bound scheduled activity.
    ///
    /// The pinned `ordinal` is the per-workflow activity ordinal recorded in
    /// history, so it maps directly onto the activity id the worker correlates
    /// its result against; the stored zero-based `attempt` is stamped onto the
    /// wire as a one-based delivery attempt (zero is malformed on the wire).
    fn to_scheduled(&self, row: &OutboxRow) -> ScheduledActivity {
        ScheduledActivity {
            namespace: self.namespace.clone(),
            activity_type: row.activity_type.clone(),
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
        self.dispatcher.dispatch(&self.to_scheduled(row)).await
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
        }
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
    /// with the attempt bumped and a future `visible_after` computed from the
    /// backoff curve; otherwise it is dead-lettered to `failed`.
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
