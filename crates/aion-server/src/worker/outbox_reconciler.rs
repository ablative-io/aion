//! Live reconciliation for stranded claimed outbox rows.
//!
//! The reconciler is dormant unless explicitly configured. When commissioned it periodically
//! re-arms only `claimed` rows whose durable claim timestamp is older than the configured
//! staleness threshold, returning them to the dispatcher's normal pending-claim path. It never
//! writes workflow history; exactly-once completion remains the Recorder's responsibility.

use std::sync::Arc;
use std::time::Duration;

use aion_store::OutboxStore;
use chrono::Utc;
use tokio::sync::watch;
use tracing::{error, info};

/// Resolved, non-optional live outbox reconciliation settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxReconcilerConfig {
    /// Interval between stale-claim reconciliation sweeps.
    pub interval: Duration,
    /// Claimed rows older than this duration are considered stranded.
    pub stale_after: Duration,
    /// Maximum claimed rows re-armed per sweep.
    pub batch_size: u32,
}

/// Periodic stale-claim reconciler for the durable outbox.
pub struct OutboxReconciler {
    store: Arc<dyn OutboxStore>,
    config: OutboxReconcilerConfig,
}

impl std::fmt::Debug for OutboxReconciler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxReconciler")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OutboxReconciler {
    /// Build a reconciler over the shared outbox store.
    #[must_use]
    pub fn new(store: Arc<dyn OutboxStore>, config: OutboxReconcilerConfig) -> Self {
        Self { store, config }
    }

    /// Run reconciliation until `shutdown` flips to true.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        info!(
            interval_ms = self.config.interval.as_millis(),
            stale_after_ms = self.config.stale_after.as_millis(),
            batch_size = self.config.batch_size,
            "outbox reconciler started"
        );
        let mut interval = tokio::time::interval(self.config.interval);
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
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        info!("outbox reconciler stopped");
    }

    async fn sweep_once(&self) {
        let now = Utc::now();
        let Some(stale_after) = chrono::Duration::from_std(self.config.stale_after).ok() else {
            error!("outbox reconciler stale_after duration is out of chrono range");
            return;
        };
        let older_than = now - stale_after;
        match self
            .store
            .rearm_stale_claimed_outbox_rows(older_than, now, self.config.batch_size)
            .await
        {
            Ok(rows) if rows.is_empty() => {}
            Ok(rows) => {
                info!(
                    rearmed = rows.len(),
                    older_than = %older_than,
                    "outbox reconciler re-armed stale claimed rows"
                );
            }
            Err(error) => {
                error!(%error, "outbox reconciler failed to re-arm stale claimed rows");
            }
        }
    }
}
