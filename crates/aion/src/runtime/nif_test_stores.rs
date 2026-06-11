//! Test-only delegating event stores modelling durability read races.

use aion_core::{Event, WorkflowId};
use aion_store::{InMemoryStore, WriteToken};

/// Delegating store whose first `stale_reads` history reads of one workflow
/// return a truncated snapshot — the race window where asynchronously
/// recorded events (a watcher's child terminal, a scope deadline's
/// `TimerFired`) land between an await's resolution read and any later read.
pub(super) struct StaleReadStore {
    inner: InMemoryStore,
    stale_workflow_id: std::sync::Mutex<WorkflowId>,
    stale_len: usize,
    stale_reads: std::sync::atomic::AtomicU32,
}

impl StaleReadStore {
    /// Build over a fresh in-memory store; reads serve full history until
    /// [`Self::set_stale_target`] arms the truncation window.
    pub(super) fn new(stale_len: usize) -> Self {
        Self {
            inner: InMemoryStore::default(),
            // Placeholder until the test learns the real workflow id.
            stale_workflow_id: std::sync::Mutex::new(WorkflowId::new_v4()),
            stale_len,
            stale_reads: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Arm `reads` truncated history reads for `workflow_id`.
    pub(super) fn set_stale_target(&self, workflow_id: &WorkflowId, reads: u32) {
        match self.stale_workflow_id.lock() {
            Ok(mut target) => *target = workflow_id.clone(),
            Err(poisoned) => *poisoned.into_inner() = workflow_id.clone(),
        }
        self.stale_reads
            .store(reads, std::sync::atomic::Ordering::Release);
    }

    fn is_stale_target(&self, workflow_id: &WorkflowId) -> bool {
        match self.stale_workflow_id.lock() {
            Ok(target) => &*target == workflow_id,
            Err(poisoned) => &*poisoned.into_inner() == workflow_id,
        }
    }
}

#[async_trait::async_trait]
impl aion_store::ReadableEventStore for StaleReadStore {
    async fn read_history(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Event>, aion_store::StoreError> {
        let mut history = self.inner.read_history(workflow_id).await?;
        if self.is_stale_target(workflow_id)
            && self
                .stale_reads
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |current| current.checked_sub(1),
                )
                .is_ok()
        {
            history.truncate(self.stale_len);
        }
        Ok(history)
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, aion_store::StoreError> {
        self.inner.read_history_from(workflow_id, from_seq).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<aion_store::RunSummary>, aion_store::StoreError> {
        self.inner.read_run_chain(workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, aion_store::StoreError> {
        self.inner.list_workflow_ids().await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, aion_store::StoreError> {
        self.inner.list_active().await
    }

    async fn query(
        &self,
        filter: &aion_core::WorkflowFilter,
    ) -> Result<Vec<aion_core::WorkflowSummary>, aion_store::StoreError> {
        self.inner.query(filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &aion_core::TimerId,
        fire_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), aion_store::StoreError> {
        self.inner
            .schedule_timer(workflow_id, timer_id, fire_at)
            .await
    }

    async fn expired_timers(
        &self,
        as_of: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<aion_store::TimerEntry>, aion_store::StoreError> {
        self.inner.expired_timers(as_of).await
    }
}

#[async_trait::async_trait]
impl aion_store::WritableEventStore for StaleReadStore {
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), aion_store::StoreError> {
        self.inner
            .append(token, workflow_id, events, expected_seq)
            .await
    }
}
