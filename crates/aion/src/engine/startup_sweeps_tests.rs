//! Tests for the optimistic terminal-deadline sweep against a concurrent public
//! writer (`reopen_workflow`) during adoption.

use std::sync::{Arc, Mutex};

use aion_core::{
    Event, EventEnvelope, PackageVersion, Payload, RunId, SearchAttributeSchema, TimerId,
    WorkflowError, WorkflowFilter, WorkflowId, WorkflowSummary,
};
use aion_store::{
    EventStore, InMemoryStore, PackageRecord, PackageRouteRecord, ReadableEventStore, RunSummary,
    StoreError, TimerEntry, WritableEventStore, WriteToken,
};
use chrono::{DateTime, Utc};
use serde_json::json;

use super::{SweepScope, sweep_uncancelled_terminal_deadlines};
use crate::durability::{Recorder, WorkflowStartRecord};
use crate::engine::startup::StartupRecoveryContext;
use crate::loader::WorkflowCatalog;
use crate::registry::Registry;
use crate::runtime::{RuntimeConfig, RuntimeHandle};
use crate::supervision::SupervisionTree;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// An [`EventStore`] wrapping an in-memory store that, on the FIRST `append` for
/// the target workflow, first commits a `WorkflowReopened` at the head the
/// caller sampled — advancing the head so the caller's own append hits a REAL
/// `SequenceConflict`. Every other operation delegates unconditionally. One-shot.
struct ReopenInjectingStore {
    inner: Arc<InMemoryStore>,
    injection: Mutex<Option<(WorkflowId, RunId)>>,
}

impl ReopenInjectingStore {
    fn new(inner: Arc<InMemoryStore>, workflow_id: WorkflowId, run_id: RunId) -> Self {
        Self {
            inner,
            injection: Mutex::new(Some((workflow_id, run_id))),
        }
    }

    /// Takes the one-shot injection, if still armed and matching `workflow_id`.
    fn take_injection(&self, workflow_id: &WorkflowId) -> Option<RunId> {
        let mut guard = self.injection.lock().ok()?;
        match guard.as_ref() {
            Some((armed_workflow, _)) if armed_workflow == workflow_id => {
                guard.take().map(|(_, run_id)| run_id)
            }
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl ReadableEventStore for ReopenInjectingStore {
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

    async fn list_paused(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_paused().await
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

#[async_trait::async_trait]
impl WritableEventStore for ReopenInjectingStore {
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        if let Some(run_id) = self.take_injection(workflow_id) {
            // Commit a reopen at the head the caller sampled, advancing it so the
            // caller's own append (below) sees the conflict.
            let reopened = Event::WorkflowReopened {
                envelope: EventEnvelope {
                    seq: expected_seq + 1,
                    recorded_at: Utc::now(),
                    workflow_id: workflow_id.clone(),
                },
                run_id,
                reopened: Vec::new(),
            };
            self.inner
                .append(
                    WriteToken::recorder(),
                    workflow_id,
                    &[reopened],
                    expected_seq,
                )
                .await?;
        }
        self.inner
            .append(token, workflow_id, events, expected_seq)
            .await
    }
}

#[async_trait::async_trait]
impl aion_store::PackageStore for ReopenInjectingStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        self.inner.put_package(record).await
    }

    async fn put_package_with_routes(
        &self,
        record: PackageRecord,
        route_workflow_types: &[String],
    ) -> Result<(), StoreError> {
        self.inner
            .put_package_with_routes(record, route_workflow_types)
            .await
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

/// Seeds a reopenable orphan: `WorkflowFailed` with an armed deadline that was
/// never cancelled (the crash window), plus its durable timer row.
async fn seed_failed_orphan(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
) -> Result<RunId, Box<dyn std::error::Error>> {
    let run_id = RunId::new_v4();
    let deadline_id = crate::time::deadline_timer_id(&run_id)?;
    let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(store));
    recorder
        .record_workflow_started(
            Utc::now(),
            WorkflowStartRecord {
                workflow_type: "checkout".to_owned(),
                input: Payload::from_json(&json!({}))?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: PackageVersion::new("a".repeat(64)),
            },
        )
        .await?;
    recorder
        .record_timer_started(Utc::now(), deadline_id, Utc::now())
        .await?;
    recorder
        .record_workflow_failed(
            Utc::now(),
            WorkflowError {
                message: "boom".to_owned(),
                details: None,
            },
        )
        .await?;
    store
        .schedule_timer(
            workflow_id,
            &crate::time::deadline_timer_id(&run_id)?,
            Utc::now(),
        )
        .await?;
    Ok(run_id)
}

fn context(
    store: &Arc<dyn EventStore>,
    runtime: &Arc<RuntimeHandle>,
    registry: &Arc<Registry>,
) -> StartupRecoveryContext {
    StartupRecoveryContext {
        store: Arc::clone(store),
        visibility_store: Arc::new(InMemoryStore::default()),
        runtime: Arc::clone(runtime),
        catalog: Arc::new(WorkflowCatalog::new()),
        registry: Arc::clone(registry),
        supervision: Arc::new(SupervisionTree::new()),
        recovery: None,
        search_attribute_schema: Arc::new(SearchAttributeSchema::new()),
        bootstrap_schedule_coordinator: false,
    }
}

/// The adoption sweep loses its append to a concurrent public `WorkflowReopened`
/// (a real `SequenceConflict`), but the COMPLETE repair predicate recognizes that
/// the run ceased to be an orphaned terminal deadline: the sweep returns Ok
/// (adoption is NOT aborted), does NOT retire the reopened run's still-live
/// deadline, and does not stale the post-reopen recorder.
#[tokio::test(flavor = "multi_thread")]
async fn adoption_sweep_yields_to_a_concurrent_reopen_without_aborting() -> TestResult {
    let inner = Arc::new(InMemoryStore::default());
    let workflow_id = WorkflowId::new_v4();
    let run_id =
        seed_failed_orphan(&(Arc::clone(&inner) as Arc<dyn EventStore>), &workflow_id).await?;
    let store: Arc<dyn EventStore> = Arc::new(ReopenInjectingStore::new(
        Arc::clone(&inner),
        workflow_id.clone(),
        run_id.clone(),
    ));
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let registry = Arc::new(Registry::default());
    let context = context(&store, &runtime, &registry);

    // (a) the sweep returns Ok — a lost append to a valid reopen does not abort.
    sweep_uncancelled_terminal_deadlines(&context, SweepScope::Adoption).await?;

    let history = store.read_history(&workflow_id).await?;
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::WorkflowReopened { .. })),
        "the injected reopen is durable: {history:#?}"
    );
    // (c) the reopened run's deadline is STILL live — the sweep did not retire it.
    assert!(
        crate::time::outstanding_deadline_timer(&history, &run_id).is_some(),
        "the sweep must not retire a reopened run's deadline: {history:#?}"
    );
    // (d) a recorder at the post-reopen head still appends — nothing was staled.
    let head = history.iter().map(Event::seq).max().unwrap_or_default();
    let mut recorder = Recorder::resume_at(
        workflow_id.clone(),
        Arc::clone(&inner) as Arc<dyn EventStore>,
        head,
    );
    recorder
        .record_workflow_completed(Utc::now(), Payload::from_json(&json!("done"))?)
        .await
        .map_err(|error| format!("the post-reopen recorder was staled: {error}"))?;
    runtime.shutdown()?;
    Ok(())
}

/// Inverse guard against over-broadening the predicate: with NO concurrent
/// writer, the same candidate IS repaired (its orphaned deadline retired).
#[tokio::test]
async fn adoption_sweep_repairs_the_candidate_without_a_concurrent_writer() -> TestResult {
    let inner = Arc::new(InMemoryStore::default());
    let store: Arc<dyn EventStore> = Arc::clone(&inner) as Arc<dyn EventStore>;
    let workflow_id = WorkflowId::new_v4();
    let run_id = seed_failed_orphan(&store, &workflow_id).await?;
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let registry = Arc::new(Registry::default());
    let context = context(&store, &runtime, &registry);

    sweep_uncancelled_terminal_deadlines(&context, SweepScope::Adoption).await?;

    let history = store.read_history(&workflow_id).await?;
    assert_eq!(
        crate::time::outstanding_deadline_timer(&history, &run_id),
        None,
        "an uncontested orphan is repaired: {history:#?}"
    );
    runtime.shutdown()?;
    Ok(())
}
