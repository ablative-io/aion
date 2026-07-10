//! Live reconciliation for stranded claimed outbox rows.
//!
//! The reconciler is dormant unless explicitly configured. When commissioned it periodically
//! re-arms only `claimed` rows whose durable claim timestamp is older than the configured
//! staleness threshold, returning them to the dispatcher's normal pending-claim path. It never
//! writes workflow history; exactly-once completion remains the Recorder's responsibility.
//!
//! # Liveness gate (#253)
//!
//! Re-arm implies live: before re-arming, each sweep probes the stale claimed candidates
//! (read-only), projects each candidate workflow's status once from event history — the SAME
//! [`status_from_events`] projection `list_active` and pause validation use, so a `Paused` run is
//! correctly live — and SETTLES the rows of terminal workflows to `Cancelled` instead of re-arming
//! them. The re-arm that follows then contractually never touches the settled rows
//! (`Cancelled` is terminal for re-arm). This closes the incident's hole: a stranded claimed row
//! for an already-`WorkflowFailed` workflow was re-armed and redelivered, and a worker served a
//! full zombie round for a dead workflow. Stale claims are anomalies, so the probe is a handful of
//! history reads per sweep — never a per-dispatch cost; the dispatcher's outbox-only determinism
//! boundary is untouched.

use std::sync::Arc;
use std::time::Duration;

use aion_core::status_from_events;
use aion_store::{EventStore, OutboxRow, OutboxStore};
use chrono::Utc;
use tokio::sync::watch;
use tracing::{error, info};

use super::outbox_settle::is_settle_terminal;

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
    /// Status probe for the liveness gate (#253): projects each stale
    /// candidate's workflow status from recorded history before any re-arm.
    event_store: Arc<dyn EventStore>,
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
    /// Build a reconciler over the shared outbox store and the event store its
    /// liveness gate projects workflow status from.
    #[must_use]
    pub fn new(
        store: Arc<dyn OutboxStore>,
        event_store: Arc<dyn EventStore>,
        config: OutboxReconcilerConfig,
    ) -> Self {
        Self {
            store,
            event_store,
            config,
        }
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

    /// One reconciliation sweep: probe → settle terminal → re-arm the rest.
    ///
    /// Any probe/settle/store error aborts the WHOLE sweep and retries next
    /// interval: a stale row waiting one extra interval is safe; re-arming a
    /// row of unknown liveness is not (the incident's exact failure).
    async fn sweep_once(&self) {
        let now = Utc::now();
        let Some(stale_after) = chrono::Duration::from_std(self.config.stale_after).ok() else {
            error!("outbox reconciler stale_after duration is out of chrono range");
            return;
        };
        let older_than = now - stale_after;
        if let Err(gate_error) = self.settle_terminal_candidates(older_than).await {
            error!(
                %gate_error,
                "outbox reconciler liveness gate failed; aborting this sweep \
                 (no row is re-armed with unknown workflow liveness)"
            );
            return;
        }
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

    /// The liveness gate (#253): settle the stale claimed rows of terminal
    /// workflows to `Cancelled` so the subsequent re-arm — which never touches
    /// `Cancelled` rows — only resurrects dispatches whose workflow was
    /// projected non-terminal at this sweep.
    async fn settle_terminal_candidates(
        &self,
        older_than: chrono::DateTime<Utc>,
    ) -> Result<(), aion_store::StoreError> {
        let stale = self
            .store
            .list_stale_claimed_outbox_rows(older_than, self.config.batch_size)
            .await?;
        if stale.is_empty() {
            return Ok(());
        }
        // Project each distinct candidate workflow ONCE per sweep.
        let mut candidates: Vec<&OutboxRow> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for row in &stale {
            if seen.insert(row.workflow_id.clone()) {
                candidates.push(row);
            }
        }
        for row in candidates {
            let history = self.event_store.read_history(&row.workflow_id).await?;
            let status = status_from_events(&history);
            if !is_settle_terminal(status) {
                continue;
            }
            let settled = self
                .store
                .cancel_outbox_rows_for_workflow(&row.workflow_id)
                .await?;
            info!(
                workflow_id = %row.workflow_id,
                projected_status = ?status,
                dispatch_keys = ?settled,
                "settled stranded outbox rows for terminal workflow; not re-armed"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use aion_core::{ContentType, Event, EventEnvelope, Payload, RunId, WorkflowError, WorkflowId};
    use aion_store::{
        ClaimScope, InMemoryStore, OutboxRow, OutboxStatus, OutboxStore, StoreError,
        WritableEventStore, WriteToken,
    };
    use async_trait::async_trait;
    use chrono::{DateTime, Duration as ChronoDuration, Utc};

    use super::{OutboxReconciler, OutboxReconcilerConfig};

    type TestError = Box<dyn std::error::Error>;

    /// In-memory [`OutboxStore`] double: real Pending/Claimed/Cancelled row
    /// state, faithful stale-claim selection and re-arm semantics (including
    /// the Cancelled-is-untouchable re-arm contract), plus an error injection
    /// switch for the probe step.
    #[derive(Default)]
    struct MockOutbox {
        rows: Mutex<Vec<OutboxRow>>,
        fail_stale_probe: AtomicBool,
        rearm_calls: Mutex<u32>,
    }

    impl MockOutbox {
        fn with_rows(rows: Vec<OutboxRow>) -> Arc<Self> {
            Arc::new(Self {
                rows: Mutex::new(rows),
                ..Self::default()
            })
        }

        fn rows(&self) -> Vec<OutboxRow> {
            self.rows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn rearm_calls(&self) -> u32 {
            *self
                .rearm_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn unsupported(method: &str) -> StoreError {
            StoreError::Backend(format!("MockOutbox does not implement {method}"))
        }
    }

    #[async_trait]
    impl OutboxStore for MockOutbox {
        async fn append_outbox_batch(&self, _rows: &[OutboxRow]) -> Result<(), StoreError> {
            Err(Self::unsupported("append_outbox_batch"))
        }

        async fn claim_outbox_rows(&self, _limit: u32) -> Result<Vec<OutboxRow>, StoreError> {
            Err(Self::unsupported("claim_outbox_rows"))
        }

        async fn claim_outbox_rows_scoped(
            &self,
            _scope: &ClaimScope,
            _limit: u32,
        ) -> Result<Vec<OutboxRow>, StoreError> {
            Err(Self::unsupported("claim_outbox_rows_scoped"))
        }

        async fn list_stale_claimed_outbox_rows(
            &self,
            older_than: DateTime<Utc>,
            limit: u32,
        ) -> Result<Vec<OutboxRow>, StoreError> {
            if self.fail_stale_probe.load(Ordering::Acquire) {
                return Err(StoreError::Backend(String::from(
                    "injected stale-probe failure",
                )));
            }
            let mut stale: Vec<OutboxRow> = self
                .rows()
                .into_iter()
                .filter(|row| {
                    row.status == OutboxStatus::Claimed
                        && row
                            .claimed_at
                            .is_some_and(|claimed_at| claimed_at < older_than)
                })
                .collect();
            stale.truncate(usize::try_from(limit).map_or(usize::MAX, |value| value));
            Ok(stale)
        }

        async fn cancel_outbox_rows_for_workflow(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<String>, StoreError> {
            let mut rows = self
                .rows
                .lock()
                .map_err(|_| StoreError::Backend(String::from("mock rows lock poisoned")))?;
            let mut settled = Vec::new();
            for row in rows.iter_mut() {
                if &row.workflow_id == workflow_id
                    && matches!(row.status, OutboxStatus::Pending | OutboxStatus::Claimed)
                {
                    row.status = OutboxStatus::Cancelled;
                    row.claimed_at = None;
                    settled.push(row.dispatch_key.clone());
                }
            }
            Ok(settled)
        }

        async fn rearm_stale_claimed_outbox_rows(
            &self,
            older_than: DateTime<Utc>,
            visible_after: DateTime<Utc>,
            limit: u32,
        ) -> Result<Vec<OutboxRow>, StoreError> {
            {
                let mut calls = self
                    .rearm_calls
                    .lock()
                    .map_err(|_| StoreError::Backend(String::from("mock calls lock poisoned")))?;
                *calls += 1;
            }
            let mut rows = self
                .rows
                .lock()
                .map_err(|_| StoreError::Backend(String::from("mock rows lock poisoned")))?;
            let mut rearmed = Vec::new();
            for row in rows.iter_mut() {
                if rearmed.len() >= usize::try_from(limit).map_or(usize::MAX, |value| value) {
                    break;
                }
                // Cancelled is contractually untouchable here — only stale
                // Claimed rows re-arm, exactly like the durable backends.
                if row.status == OutboxStatus::Claimed
                    && row
                        .claimed_at
                        .is_some_and(|claimed_at| claimed_at < older_than)
                {
                    row.status = OutboxStatus::Pending;
                    row.visible_after = visible_after;
                    row.claimed_at = None;
                    rearmed.push(row.clone());
                }
            }
            Ok(rearmed)
        }

        async fn complete_outbox_row(&self, _dispatch_key: &str) -> Result<(), StoreError> {
            Err(Self::unsupported("complete_outbox_row"))
        }

        async fn retry_outbox_row(
            &self,
            _dispatch_key: &str,
            _next_attempt: u32,
            _visible_after: DateTime<Utc>,
        ) -> Result<(), StoreError> {
            Err(Self::unsupported("retry_outbox_row"))
        }

        async fn fail_outbox_row(&self, _dispatch_key: &str) -> Result<(), StoreError> {
            Err(Self::unsupported("fail_outbox_row"))
        }

        async fn count_inflight_outbox_rows(&self, _namespace: &str) -> Result<u64, StoreError> {
            Err(Self::unsupported("count_inflight_outbox_rows"))
        }

        async fn count_claimed_outbox_rows(&self, _namespace: &str) -> Result<u64, StoreError> {
            Err(Self::unsupported("count_claimed_outbox_rows"))
        }

        async fn pending_outbox_routes(&self) -> Result<Vec<ClaimScope>, StoreError> {
            Err(Self::unsupported("pending_outbox_routes"))
        }
    }

    fn envelope(workflow_id: &WorkflowId, seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    fn started(workflow_id: &WorkflowId, seq: u64) -> Result<Event, TestError> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(workflow_id, seq),
            workflow_type: String::from("checkout"),
            input: Payload::from_json(&serde_json::json!({}))?,
            run_id: RunId::new_v4(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn failed(workflow_id: &WorkflowId, seq: u64) -> Event {
        Event::WorkflowFailed {
            envelope: envelope(workflow_id, seq),
            error: WorkflowError {
                message: String::from("boom"),
                details: None,
            },
        }
    }

    fn paused(workflow_id: &WorkflowId, seq: u64) -> Event {
        Event::WorkflowPaused {
            envelope: envelope(workflow_id, seq),
            run_id: RunId::new_v4(),
            reason: None,
            operator: None,
        }
    }

    fn stale_claimed_row(workflow_id: &WorkflowId, ordinal: u64) -> OutboxRow {
        let staled_at = Utc::now() - ChronoDuration::seconds(3_600);
        let mut row = OutboxRow::pending(
            workflow_id.clone(),
            ordinal,
            String::from("dev_brief"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            staled_at,
        );
        row.status = OutboxStatus::Claimed;
        row.claimed_at = Some(staled_at);
        row
    }

    fn config() -> OutboxReconcilerConfig {
        OutboxReconcilerConfig {
            interval: std::time::Duration::from_millis(50),
            stale_after: std::time::Duration::from_millis(100),
            batch_size: 16,
        }
    }

    async fn seeded_event_store(
        histories: &[(&WorkflowId, Vec<Event>)],
    ) -> Result<Arc<InMemoryStore>, TestError> {
        let store = Arc::new(InMemoryStore::default());
        for (workflow_id, events) in histories {
            store
                .append(WriteToken::recorder(), workflow_id, events, 0)
                .await?;
        }
        Ok(store)
    }

    /// The incident replay at unit scale: a stale claimed row whose workflow's
    /// history ends `WorkflowFailed` is SETTLED to `Cancelled` — never
    /// re-armed, so no dispatcher can ever deliver it.
    #[tokio::test]
    async fn stale_row_of_terminal_workflow_is_settled_not_rearmed() -> Result<(), TestError> {
        let workflow_id = WorkflowId::new_v4();
        let outbox = MockOutbox::with_rows(vec![stale_claimed_row(&workflow_id, 0)]);
        let event_store = seeded_event_store(&[(
            &workflow_id,
            vec![started(&workflow_id, 1)?, failed(&workflow_id, 2)],
        )])
        .await?;
        let reconciler = OutboxReconciler::new(
            Arc::clone(&outbox) as Arc<dyn OutboxStore>,
            event_store,
            config(),
        );

        reconciler.sweep_once().await;

        let rows = outbox.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].status,
            OutboxStatus::Cancelled,
            "the terminal workflow's stranded row must settle, not re-arm"
        );
        Ok(())
    }

    /// A stale claimed row of a genuinely `Running` workflow re-arms exactly
    /// as before the gate.
    #[tokio::test]
    async fn stale_row_of_running_workflow_is_rearmed() -> Result<(), TestError> {
        let workflow_id = WorkflowId::new_v4();
        let outbox = MockOutbox::with_rows(vec![stale_claimed_row(&workflow_id, 0)]);
        let event_store =
            seeded_event_store(&[(&workflow_id, vec![started(&workflow_id, 1)?])]).await?;
        let reconciler = OutboxReconciler::new(
            Arc::clone(&outbox) as Arc<dyn OutboxStore>,
            event_store,
            config(),
        );

        reconciler.sweep_once().await;

        let rows = outbox.rows();
        assert_eq!(
            rows[0].status,
            OutboxStatus::Pending,
            "a live workflow's stranded row must return to the claim path"
        );
        Ok(())
    }

    /// `Paused` is LIVE (held, not dead): the row re-arms — the #204 hold at
    /// claim time keeps it un-claimed for the paused window. Settling it would
    /// wrongly retire a resumable run's dispatch.
    #[tokio::test]
    async fn stale_row_of_paused_workflow_is_rearmed_not_settled() -> Result<(), TestError> {
        let workflow_id = WorkflowId::new_v4();
        let outbox = MockOutbox::with_rows(vec![stale_claimed_row(&workflow_id, 0)]);
        let event_store = seeded_event_store(&[(
            &workflow_id,
            vec![started(&workflow_id, 1)?, paused(&workflow_id, 2)],
        )])
        .await?;
        let reconciler = OutboxReconciler::new(
            Arc::clone(&outbox) as Arc<dyn OutboxStore>,
            event_store,
            config(),
        );

        reconciler.sweep_once().await;

        let rows = outbox.rows();
        assert_eq!(
            rows[0].status,
            OutboxStatus::Pending,
            "a Paused run is live; its stranded row must re-arm (the #204 hold gates the claim)"
        );
        Ok(())
    }

    /// A probe error aborts the WHOLE sweep: nothing is settled, nothing is
    /// re-armed, the row stays Claimed for the next interval. A stale row
    /// waiting one extra interval is safe; re-arming a row of unknown
    /// liveness is not.
    #[tokio::test]
    async fn probe_error_aborts_the_sweep_leaving_the_row_claimed() -> Result<(), TestError> {
        let workflow_id = WorkflowId::new_v4();
        let outbox = MockOutbox::with_rows(vec![stale_claimed_row(&workflow_id, 0)]);
        outbox.fail_stale_probe.store(true, Ordering::Release);
        let event_store =
            seeded_event_store(&[(&workflow_id, vec![started(&workflow_id, 1)?])]).await?;
        let reconciler = OutboxReconciler::new(
            Arc::clone(&outbox) as Arc<dyn OutboxStore>,
            event_store,
            config(),
        );

        reconciler.sweep_once().await;

        assert_eq!(
            outbox.rearm_calls(),
            0,
            "a gate failure must abort the sweep before any re-arm"
        );
        assert_eq!(outbox.rows()[0].status, OutboxStatus::Claimed);

        // Next interval, probe healthy again: the row recovers normally.
        outbox.fail_stale_probe.store(false, Ordering::Release);
        reconciler.sweep_once().await;
        assert_eq!(outbox.rows()[0].status, OutboxStatus::Pending);
        Ok(())
    }

    /// Multiple workflows in one sweep: each candidate is projected once and
    /// only the terminal one settles; the live one re-arms.
    #[tokio::test]
    async fn mixed_sweep_settles_terminal_and_rearms_live() -> Result<(), TestError> {
        let dead = WorkflowId::new_v4();
        let live = WorkflowId::new_v4();
        let outbox = MockOutbox::with_rows(vec![
            stale_claimed_row(&dead, 0),
            stale_claimed_row(&live, 0),
        ]);
        let event_store = seeded_event_store(&[
            (&dead, vec![started(&dead, 1)?, failed(&dead, 2)]),
            (&live, vec![started(&live, 1)?]),
        ])
        .await?;
        let reconciler = OutboxReconciler::new(
            Arc::clone(&outbox) as Arc<dyn OutboxStore>,
            event_store,
            config(),
        );

        reconciler.sweep_once().await;

        let statuses: Vec<(String, OutboxStatus)> = outbox
            .rows()
            .into_iter()
            .map(|row| (row.workflow_id.to_string(), row.status))
            .collect();
        assert!(statuses.contains(&(dead.to_string(), OutboxStatus::Cancelled)));
        assert!(statuses.contains(&(live.to_string(), OutboxStatus::Pending)));
        Ok(())
    }
}
