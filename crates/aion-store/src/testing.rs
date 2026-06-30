//! Recording test doubles for store decorators.
//!
//! [`ShardSeamSpy`] is a thin recording wrapper over [`InMemoryStore`]: every
//! non-seam [`crate::EventStore`] method delegates to the inner store (so the spy is a
//! correct, panic-free store), while every DEFAULT-BODIED seam method — the six
//! per-shard owned-shard seams on [`ReadableEventStore`] AND the two outbox-recovery
//! seams on [`WritableEventStore`] (`rearm_outbox_pending`, `settle_outbox_row_cancelled`)
//! — is overridden to RECORD its call and return a DISTINCTIVE value. The singular
//! failover seams (`acquire_owned_shard`, `is_current_owner`, `publish_shard_owner`)
//! and `settle_outbox_row_cancelled` return sentinels that differ from the trait's
//! silent no-op defaults, so a decorator that inherits a default instead of
//! forwarding is unambiguously detectable by BOTH the returned value and the
//! missing recorded call (this is exactly the #157-class hazard the spy exists to
//! catch).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::memory::InMemoryStore;
use crate::package::{PackageRecord, PackageRouteRecord, PackageStore};
use crate::{
    Event, OutboxRow, ReadableEventStore, RunSummary, StoreError, TimerEntry, TimerId,
    WorkflowFilter, WorkflowId, WorkflowSummary, WritableEventStore, WriteToken,
};

/// Recording [`crate::EventStore`] double that delegates to an inner [`InMemoryStore`]
/// and records every owned-shard seam call into a shared call-log.
///
/// The three singular failover-seam methods return sentinels distinct from the
/// trait defaults, so a decorator that fails to forward them is observable both
/// by the returned value and by the absence of its recorded call.
pub struct ShardSeamSpy {
    inner: InMemoryStore,
    calls: Arc<Mutex<Vec<String>>>,
}

impl ShardSeamSpy {
    /// Construct a spy wrapping a fresh, empty [`InMemoryStore`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: InMemoryStore::default(),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Snapshot of the recorded seam calls, in invocation order.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Append `entry` to the shared call-log.
    fn record(&self, entry: String) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry);
    }
}

impl Default for ShardSeamSpy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ReadableEventStore for ShardSeamSpy {
    fn set_owned_shards(&self, shards: Option<&[usize]>) {
        self.record(format!("set_owned_shards:{shards:?}"));
        self.inner.set_owned_shards(shards);
    }

    fn acquire_owned_shards(&self, shards: &[usize]) -> Result<(), StoreError> {
        self.record(format!("acquire_owned_shards:{shards:?}"));
        self.inner.acquire_owned_shards(shards)
    }

    fn acquire_owned_shard(&self, shard: usize) -> Result<(), StoreError> {
        self.record(format!("acquire_owned_shard:{shard}"));
        Err(StoreError::NotOwner { shard })
    }

    fn extend_owned_shards(&self, shards: &[usize]) {
        self.record(format!("extend_owned_shards:{shards:?}"));
        self.inner.extend_owned_shards(shards);
    }

    fn is_current_owner(&self, shard: usize) -> bool {
        self.record(format!("is_current_owner:{shard}"));
        false
    }

    fn publish_shard_owner(&self, shard: usize) -> Result<(), StoreError> {
        self.record(format!("publish_shard_owner:{shard}"));
        Err(StoreError::NotOwner { shard })
    }

    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history(workflow_id).await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history_from(workflow_id, from_seq).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.inner.read_run_chain(workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_workflow_ids().await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_active().await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        self.inner.query(filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner
            .schedule_timer(workflow_id, timer_id, fire_at)
            .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        self.inner.expired_timers(as_of).await
    }
}

#[async_trait]
impl WritableEventStore for ShardSeamSpy {
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        self.inner
            .append(token, workflow_id, events, expected_seq)
            .await
    }

    async fn append_with_outbox(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[OutboxRow],
    ) -> Result<(), StoreError> {
        self.inner
            .append_with_outbox(token, workflow_id, events, expected_seq, outbox_rows)
            .await
    }

    /// Record + report success. The trait default REFUSES a non-empty re-arm, so
    /// a decorator that fails to forward (hitting that default) is caught by the
    /// missing recorded call.
    async fn rearm_outbox_pending(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        self.record(format!("rearm_outbox_pending:{}", rows.len()));
        Ok(())
    }

    /// Record + return an `Err` sentinel distinct from the trait's silent `Ok(())`
    /// no-op default, so a decorator that drops this seam — the #157-class hazard
    /// that strands cancelled fan-out rows — is caught by BOTH the returned value
    /// and the missing recorded call.
    async fn settle_outbox_row_cancelled(&self, dispatch_key: &str) -> Result<(), StoreError> {
        self.record(format!("settle_outbox_row_cancelled:{dispatch_key}"));
        Err(StoreError::Backend(String::from(
            "ShardSeamSpy::settle_outbox_row_cancelled sentinel",
        )))
    }
}

#[async_trait]
impl PackageStore for ShardSeamSpy {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        self.inner.put_package(record).await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        self.inner.list_packages().await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner.delete_package(workflow_type, content_hash).await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner
            .put_package_route(workflow_type, content_hash)
            .await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        self.inner.list_package_routes().await
    }
}
