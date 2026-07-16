//! Startup recovery sweep tests (split from `startup.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use aion_core::{
    Event, EventEnvelope, Payload, RunId, SearchAttributeSchema, WorkflowId, WorkflowStatus,
};
use aion_store::{EventStore, StoreError};
use chrono::Utc;
use serde_json::json;

use super::{StartupRecoveryContext, sweep_continued_as_new_replacements};
use crate::EngineError;
use crate::loader::WorkflowCatalog;
use crate::registry::Registry;
use crate::runtime::{RuntimeConfig, RuntimeHandle};
use crate::supervision::SupervisionTree;
use aion_store::InMemoryStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Canned store modelling the N-5 race: the first history read shows a
/// stranded continue-as-new run (no successor `WorkflowStarted`); once
/// `successor_appears_after_reads` reads have happened, the racing exit
/// monitor's successor start is visible. Appends are never expected —
/// the sweep's own start fails earlier (no loadable type), standing in
/// for the race loser's `SequenceConflict`.
struct RacingSuccessorStore {
    workflow_id: WorkflowId,
    base_history: Vec<Event>,
    full_history: Vec<Event>,
    successor_appears_after_reads: u32,
    reads: AtomicU32,
    appears: bool,
}

#[async_trait::async_trait]
impl aion_store::ReadableEventStore for RacingSuccessorStore {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        if workflow_id != &self.workflow_id {
            return Ok(Vec::new());
        }
        let read = self.reads.fetch_add(1, Ordering::AcqRel) + 1;
        if self.appears && read > self.successor_appears_after_reads {
            Ok(self.full_history.clone())
        } else {
            Ok(self.base_history.clone())
        }
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        let _ = (workflow_id, from_seq);
        Err(StoreError::Backend(
            "unexpected read_history_from in the sweep test".to_owned(),
        ))
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<aion_store::RunSummary>, StoreError> {
        let _ = workflow_id;
        Err(StoreError::Backend(
            "unexpected read_run_chain in the sweep test".to_owned(),
        ))
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        Ok(vec![self.workflow_id.clone()])
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        Ok(Vec::new())
    }

    async fn list_paused(&self) -> Result<Vec<WorkflowId>, StoreError> {
        Ok(Vec::new())
    }

    async fn query(
        &self,
        filter: &aion_core::WorkflowFilter,
    ) -> Result<Vec<aion_core::WorkflowSummary>, StoreError> {
        if filter.status != Some(WorkflowStatus::ContinuedAsNew) {
            return Ok(Vec::new());
        }
        Ok(vec![aion_core::WorkflowSummary {
            workflow_id: self.workflow_id.clone(),
            workflow_type: "checkout".to_owned(),
            status: WorkflowStatus::ContinuedAsNew,
            started_at: Utc::now(),
            ended_at: None,
            parent: None,
            failed_step: None,
            failure_reason: None,
        }])
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &aion_core::TimerId,
        fire_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StoreError> {
        let _ = (workflow_id, timer_id, fire_at);
        Err(StoreError::Backend(
            "unexpected schedule_timer in the sweep test".to_owned(),
        ))
    }

    async fn expired_timers(
        &self,
        as_of: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<aion_store::TimerEntry>, StoreError> {
        let _ = as_of;
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl aion_store::WritableEventStore for RacingSuccessorStore {
    async fn append(
        &self,
        token: aion_store::WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        let _ = (token, workflow_id, events, expected_seq);
        Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found: expected_seq + 1,
        })
    }
}

/// The sweep never touches deployed packages: reads are legitimately empty,
/// mutations are unexpected.
#[async_trait::async_trait]
impl aion_store::PackageStore for RacingSuccessorStore {
    async fn put_package(&self, record: aion_store::PackageRecord) -> Result<(), StoreError> {
        let _ = record;
        Err(StoreError::Backend(
            "unexpected put_package in the sweep test".to_owned(),
        ))
    }

    async fn put_package_with_routes(
        &self,
        record: aion_store::PackageRecord,
        route_workflow_types: &[String],
    ) -> Result<(), StoreError> {
        let _ = (record, route_workflow_types);
        Err(StoreError::Backend(
            "unexpected put_package_with_routes in the sweep test".to_owned(),
        ))
    }

    async fn list_packages(&self) -> Result<Vec<aion_store::PackageRecord>, StoreError> {
        Ok(Vec::new())
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let _ = (workflow_type, content_hash);
        Err(StoreError::Backend(
            "unexpected delete_package in the sweep test".to_owned(),
        ))
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let _ = (workflow_type, content_hash);
        Err(StoreError::Backend(
            "unexpected put_package_route in the sweep test".to_owned(),
        ))
    }

    async fn list_package_routes(&self) -> Result<Vec<aion_store::PackageRouteRecord>, StoreError> {
        Ok(Vec::new())
    }
}

/// `(base history, history with the successor, continued run id)`.
type StrandedHistories = (Vec<Event>, Vec<Event>, RunId);

fn stranded_histories(
    workflow_id: &WorkflowId,
) -> Result<StrandedHistories, Box<dyn std::error::Error>> {
    let first_run = RunId::new_v4();
    let second_run = RunId::new_v4();
    let envelope = |seq: u64| EventEnvelope {
        seq,
        recorded_at: Utc::now(),
        workflow_id: workflow_id.clone(),
    };
    let input = Payload::from_json(&json!({"next": true}))?;
    let base = vec![
        Event::WorkflowStarted {
            envelope: envelope(1),
            workflow_type: "checkout".to_owned(),
            input: Payload::from_json(&json!({"first": true}))?,
            run_id: first_run.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::WorkflowContinuedAsNew {
            envelope: envelope(2),
            input: input.clone(),
            workflow_type: None,
            parent_run_id: first_run.clone(),
        },
    ];
    let mut full = base.clone();
    full.push(Event::WorkflowStarted {
        envelope: envelope(3),
        workflow_type: "checkout".to_owned(),
        input,
        run_id: second_run,
        parent_run_id: Some(first_run.clone()),
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    });
    Ok((base, full, first_run))
}

fn recovery_context(
    store: Arc<dyn EventStore>,
    runtime: Arc<RuntimeHandle>,
    catalog: Arc<WorkflowCatalog>,
) -> StartupRecoveryContext {
    StartupRecoveryContext {
        store,
        visibility_store: Arc::new(InMemoryStore::default()),
        runtime,
        catalog,
        registry: Arc::new(Registry::default()),
        supervision: Arc::new(SupervisionTree::new()),
        recovery: None,
        search_attribute_schema: Arc::new(SearchAttributeSchema::new()),
        bootstrap_schedule_coordinator: true,
    }
}

/// N-5: the sweep's start races the recovered run's exit monitor, which
/// starts the same successor concurrently. When the sweep's start fails
/// but a re-read shows the successor `WorkflowStarted` durable (the
/// winner's append), the failure is benign and `EngineBuilder::build`
/// must not fail. Before the fix the sweep propagated the loser's error
/// and the whole build failed on a `SequenceConflict`-class race.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_start_race_lost_to_the_exit_monitor_is_benign() -> TestResult {
    let workflow_id = WorkflowId::new_v4();
    let (base, full, _continued) = stranded_histories(&workflow_id)?;
    let store = Arc::new(RacingSuccessorStore {
        workflow_id,
        base_history: base,
        full_history: full,
        // Read #1 is the sweep's pre-start read (no successor yet); the
        // racing monitor wins during the sweep's failed start, so the
        // post-failure re-read (#2) sees the successor.
        successor_appears_after_reads: 1,
        reads: AtomicU32::new(0),
        appears: true,
    });
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let catalog = Arc::new(WorkflowCatalog::new());
    let context = recovery_context(store as Arc<dyn EventStore>, Arc::clone(&runtime), catalog);

    sweep_continued_as_new_replacements(&context)
        .await
        .map_err(|error| format!("a lost start race must be benign (N-5): {error}"))?;
    runtime.shutdown()?;
    Ok(())
}

/// The guard must not swallow real failures: a start failure with NO
/// durable successor is a genuine fault and still fails the build.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_start_failure_without_a_successor_still_fails() -> TestResult {
    let workflow_id = WorkflowId::new_v4();
    let (base, full, _continued) = stranded_histories(&workflow_id)?;
    let store = Arc::new(RacingSuccessorStore {
        workflow_id,
        base_history: base,
        full_history: full,
        successor_appears_after_reads: u32::MAX,
        reads: AtomicU32::new(0),
        appears: false,
    });
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let catalog = Arc::new(WorkflowCatalog::new());
    let context = recovery_context(store as Arc<dyn EventStore>, Arc::clone(&runtime), catalog);

    let result = sweep_continued_as_new_replacements(&context).await;
    assert!(
        matches!(result, Err(EngineError::WorkflowNotFound { .. })),
        "a start failure without a durable successor must propagate: {result:?}"
    );
    runtime.shutdown()?;
    Ok(())
}
