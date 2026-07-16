//! Runtime-deploy durability e2e: packages loaded through the live
//! `Engine::load_package` deploy seam must survive an engine restart on the
//! same store, so startup recovery can resolve every run's recorded pinned
//! version WITHOUT the operator re-supplying `--workflow-package`.
//!
//! These tests cover the P0 durability gap where a `kill -9` after a runtime
//! deploy stranded mid-flight runs: history listed them as Running, but
//! recovery skipped them because the deployed package existed only in
//! process memory.

#[path = "common/reload_fixture.rs"]
mod reload_fixture;

use std::{sync::Arc, time::Duration};

use aion::{EngineError, PinHolder};
use aion_package::{ExtractionLimits, Package, PackageBuilder, WorkflowEntry};
use aion_store::{
    Event, EventStore, InMemoryStore, OutboxRow, PackageRecord, PackageRouteRecord, PackageStore,
    ReadableEventStore, RunSummary, StoreError, TimerEntry, TimerId, WorkflowFilter, WorkflowId,
    WorkflowSummary, WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Notify;

use reload_fixture::{
    RELOAD_MODULE, compile_reload_beam, engine_with, input, recorded_version, reload_package,
    result_int, start, version_of,
};

struct PausingPackageStore {
    inner: InMemoryStore,
    entered: Notify,
    release: Notify,
}

impl PausingPackageStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: InMemoryStore::default(),
            entered: Notify::new(),
            release: Notify::new(),
        })
    }
}

#[async_trait]
impl ReadableEventStore for PausingPackageStore {
    async fn read_history(&self, id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history(id).await
    }
    async fn read_history_from(&self, id: &WorkflowId, seq: u64) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history_from(id, seq).await
    }
    async fn read_run_chain(&self, id: &WorkflowId) -> Result<Vec<RunSummary>, StoreError> {
        self.inner.read_run_chain(id).await
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

#[async_trait]
impl WritableEventStore for PausingPackageStore {
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
}

#[async_trait]
impl PackageStore for PausingPackageStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        let primary = record.workflow_type.clone();
        self.put_package_with_routes(record, &[primary]).await
    }

    async fn put_package_with_routes(
        &self,
        record: PackageRecord,
        route_workflow_types: &[String],
    ) -> Result<(), StoreError> {
        drop(record);
        let route_count = route_workflow_types.len();
        self.entered.notify_one();
        self.release.notified().await;
        Err(StoreError::Backend(format!(
            "injected put_package failure for {route_count} routes"
        )))
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

#[tokio::test]
async fn failed_paused_persistence_never_publishes_a_startable_hash() -> TestResult {
    let package = reload_package(&compile_reload_beam(1)?, "run")?;
    let store_impl = PausingPackageStore::new();
    let store: Arc<dyn EventStore> = store_impl.clone();
    let engine = Arc::new(engine_with(&store, vec![]).await?);
    let deploy_engine = Arc::clone(&engine);
    let deploy = tokio::spawn(async move { deploy_engine.load_package(package).await });

    store_impl.entered.notified().await;
    let staged_versions = engine.list_workflow_versions()?;
    assert!(
        staged_versions.iter().all(|version| !version.route_active),
        "staged versions published before persistence: {staged_versions:#?}"
    );
    let raced_start = start(&engine).await;
    assert!(
        raced_start.is_err(),
        "staged hash became startable before persistence: {staged_versions:#?}"
    );
    for workflow_id in store.list_workflow_ids().await? {
        let history = store.read_history(&workflow_id).await?;
        assert!(
            !history
                .iter()
                .any(|event| matches!(event, Event::WorkflowStarted { workflow_type, .. } if workflow_type == RELOAD_MODULE)),
            "racing start recorded the staged package pin before persistence: {history:#?}"
        );
    }
    store_impl.release.notify_one();
    let deploy_result = deploy.await?;
    let Err(deploy_error) = deploy_result else {
        return Err("injected put_package failure unexpectedly succeeded".into());
    };
    assert!(
        deploy_error
            .to_string()
            .contains("injected put_package failure")
    );
    assert!(engine.list_workflow_versions()?.is_empty());
    engine.shutdown()?;
    drop(engine);

    let recovered = engine_with(&store, vec![]).await?;
    assert!(
        recovered.list_workflow_versions()?.is_empty(),
        "failed deploy resurrected after restart"
    );
    for workflow_id in store.list_workflow_ids().await? {
        assert!(
            !store
                .read_history(&workflow_id)
                .await?
                .iter()
                .any(|event| matches!(event, Event::WorkflowStarted { workflow_type, .. } if workflow_type == RELOAD_MODULE)),
            "racing start left an unrecoverable staged package pin after restart"
        );
    }
    recovered.shutdown()?;
    Ok(())
}

fn grouped_package(
    package: &Package,
    child_type: &str,
) -> Result<Package, Box<dyn std::error::Error>> {
    let mut manifest = package.manifest().clone();
    manifest.additional_workflows.push(WorkflowEntry {
        workflow_type: child_type.to_owned(),
        entry_module: manifest.entry_module.clone(),
        entry_function: "gated".to_owned(),
        input_schema: manifest.input_schema.clone(),
        output_schema: manifest.output_schema.clone(),
        timeout: manifest.timeout,
        internal: true,
    });
    let bytes = PackageBuilder::new(manifest, package.beams().clone()).write_to_bytes()?;
    Ok(Package::load_from_bytes(
        bytes,
        ExtractionLimits::unbounded(),
    )?)
}

#[tokio::test]
async fn active_sibling_pins_whole_archive_group_across_restart() -> TestResult {
    const CHILD_TYPE: &str = "aion_internal_awl_child_reload_group_fan_0";
    let v1 = grouped_package(
        &reload_package(&compile_reload_beam(1)?, "run")?,
        CHILD_TYPE,
    )?;
    let v2 = grouped_package(
        &reload_package(&compile_reload_beam(2)?, "run")?,
        CHILD_TYPE,
    )?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![]).await?;
    engine.load_package(v1.clone()).await?;
    let child = engine
        .start_workflow(
            CHILD_TYPE,
            input()?,
            std::collections::HashMap::new(),
            "default".to_owned(),
        )
        .await?;
    let child_id = child.workflow_id().clone();
    let child_run = child.run_id().clone();
    engine.load_package(v2.clone()).await?;

    let refusal = engine
        .unload_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await;
    assert!(
        matches!(
            refusal,
            Err(EngineError::VersionPinned {
                ref workflow_type,
                pinned_by: PinHolder::LiveRun { .. },
                ..
            }) if workflow_type == CHILD_TYPE
        ),
        "active sibling did not pin its whole archive group: {refusal:?}"
    );
    assert!(engine.list_workflow_versions()?.iter().any(|entry| {
        entry.workflow_type == CHILD_TYPE && entry.content_hash == *v1.content_hash()
    }));
    engine.shutdown()?;

    let recovered = engine_with(&store, vec![]).await?;
    let versions = recovered.list_workflow_versions()?;
    assert!(versions.iter().any(|entry| {
        entry.workflow_type == RELOAD_MODULE && entry.content_hash == *v1.content_hash()
    }));
    assert!(versions.iter().any(|entry| {
        entry.workflow_type == CHILD_TYPE && entry.content_hash == *v1.content_hash()
    }));
    let recovered_child = recovered
        .registry()
        .get(&child_id, &child_run)?
        .ok_or("active sibling did not recover after refused group unload")?;
    assert_eq!(recovered_child.loaded_version(), v1.content_hash());
    recovered.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn group_redeploy_durably_supersedes_an_old_explicit_child_route() -> TestResult {
    const CHILD_TYPE: &str = "aion_internal_awl_child_reload_group_fan_0";
    let v1 = grouped_package(
        &reload_package(&compile_reload_beam(1)?, "run")?,
        CHILD_TYPE,
    )?;
    let v2 = grouped_package(
        &reload_package(&compile_reload_beam(2)?, "run")?,
        CHILD_TYPE,
    )?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![]).await?;
    engine.load_package(v1.clone()).await?;
    engine.load_package(v2.clone()).await?;
    engine
        .route_workflow_version(CHILD_TYPE, v1.content_hash())
        .await?;
    assert_route(&engine, CHILD_TYPE, v1.content_hash())?;

    // An idempotent group redeploy is routing intent for every member. Its
    // one durable write must supersede the stale child route across restart.
    engine.load_package(v2.clone()).await?;
    engine.shutdown()?;

    let recovered = engine_with(&store, vec![]).await?;
    assert_route(&recovered, RELOAD_MODULE, v2.content_hash())?;
    assert_route(&recovered, CHILD_TYPE, v2.content_hash())?;
    recovered.shutdown()?;
    Ok(())
}

fn assert_route(
    engine: &aion::Engine,
    workflow_type: &str,
    expected: &aion_package::ContentHash,
) -> TestResult {
    let versions = engine.list_workflow_versions()?;
    assert!(
        versions.iter().any(|entry| {
            entry.workflow_type == workflow_type
                && entry.content_hash == *expected
                && entry.route_active
        }),
        "workflow `{workflow_type}` is not routed to `{expected}`: {versions:#?}"
    );
    Ok(())
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

// --- the P0 repro: deploy → mid-flight → crash/restart → run recovers -------

/// A package deployed at runtime (no startup `--workflow-package`) must be
/// reloaded from the store on restart so a mid-flight run pinned to it
/// recovers, accepts its remaining signal, and completes.
#[tokio::test]
async fn runtime_deployed_package_survives_restart_and_recovers_runs() -> TestResult {
    let v1 = reload_package(&compile_reload_beam(1)?, "gated")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    // Epoch 1: the engine boots EMPTY; v1 arrives through the runtime deploy
    // seam, a run starts and records mid-flight progress, then the engine
    // stops without completing it.
    let engine = engine_with(&store, vec![]).await?;
    let outcome = engine.load_package(v1.clone()).await?;
    assert!(outcome.freshly_loaded, "deploy must register the version");
    let (workflow_id, run_id) = start(&engine).await?;
    engine
        .signal(&workflow_id, &run_id, "step", input()?)
        .await?;
    engine.shutdown()?;

    // Epoch 2: same store, NO workflow packages supplied at startup.
    let recovered = engine_with(&store, vec![]).await?;
    let handle = recovered
        .registry()
        .get(&workflow_id, &run_id)?
        .ok_or("run pinned to the runtime-deployed package must recover after restart")?;
    assert_eq!(
        handle.loaded_version(),
        v1.content_hash(),
        "recovery must resolve the recorded deployed version"
    );

    // The recovered run is live: the remaining signal lands and it completes
    // — and its durable pin still names the deployed version.
    recovered
        .signal(&workflow_id, &run_id, "release", input()?)
        .await?;
    assert_eq!(result_int(&recovered, &workflow_id, &run_id).await?, 1);
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(
        recorded_version(&history, &run_id)?,
        version_of(&v1),
        "the recorded package pin must survive the restart"
    );

    // And the deployed version remains route-active for new starts.
    let (new_id, new_run) = start(&recovered).await?;
    recovered
        .signal(&new_id, &new_run, "step", input()?)
        .await?;
    recovered
        .signal(&new_id, &new_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&recovered, &new_id, &new_run).await?, 1);
    recovered.shutdown()?;
    Ok(())
}

/// Regression: before timeout identity preservation in `to_archive_bytes`,
/// the stored archive was silently restamped beams-only and restart skipped
/// the explicit-timeout version because it no longer matched its store key.
#[tokio::test]
async fn explicit_timeout_deploy_survives_restart_under_the_same_store_key() -> TestResult {
    let legacy = reload_package(&compile_reload_beam(1)?, "run")?;
    let mut manifest = legacy.manifest().clone();
    manifest.timeout = Duration::new(7_200, 500_000_000);
    let archive = PackageBuilder::new(manifest, legacy.beams().clone())
        .with_explicit_timeout_identity()
        .write_to_bytes()?;
    let package = Package::load_from_bytes(archive, ExtractionLimits::unbounded())?;
    let expected = package.content_hash().clone();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![]).await?;
    engine.load_package(package).await?;
    let persisted = store.list_packages().await?;
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].content_hash, expected.to_string());
    let stored_package =
        Package::load_from_bytes(&persisted[0].archive, ExtractionLimits::unbounded())?;
    assert_eq!(stored_package.content_hash(), &expected);
    engine.shutdown()?;

    let recovered = engine_with(&store, vec![]).await?;
    let versions = recovered.list_workflow_versions()?;
    assert!(
        versions
            .iter()
            .any(|version| version.content_hash == expected),
        "explicit-timeout version was not restored under its persisted key: {versions:#?}"
    );
    recovered.shutdown()?;
    Ok(())
}

// --- routing pointer durability ----------------------------------------------

/// An explicit route re-point (rollback) made at runtime must survive a
/// restart: with v1 and v2 both deployed and the route rolled back to v1,
/// the restarted engine routes new starts to v1, not the latest deploy.
#[tokio::test]
async fn route_pointer_survives_restart() -> TestResult {
    let v1 = reload_package(&compile_reload_beam(1)?, "run")?;
    let v2 = reload_package(&compile_reload_beam(2)?, "run")?;
    assert_ne!(v1.content_hash(), v2.content_hash());
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![]).await?;
    engine.load_package(v1.clone()).await?;
    engine.load_package(v2.clone()).await?;
    // Roll the route back to v1 — the durable pointer, not deploy order,
    // is the routing truth.
    engine
        .route_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await?;
    engine.shutdown()?;

    let recovered = engine_with(&store, vec![]).await?;
    let versions = recovered.list_workflow_versions()?;
    assert_eq!(
        versions.len(),
        2,
        "both deployed versions must reload: {versions:#?}"
    );
    let routed = versions
        .iter()
        .find(|version| version.route_active)
        .ok_or("a reloaded version must be route-active")?;
    assert_eq!(
        &routed.content_hash,
        v1.content_hash(),
        "the rolled-back route pointer must survive the restart"
    );

    // Proof by execution: a new start runs v1 behavior.
    let (workflow_id, run_id) = start(&recovered).await?;
    assert_eq!(result_int(&recovered, &workflow_id, &run_id).await?, 1);
    recovered.shutdown()?;
    Ok(())
}

// --- unload removes the persisted artifact -----------------------------------

/// Unloading a version must delete its persisted archive: after a restart
/// the unloaded version is gone while the surviving version still loads.
#[tokio::test]
async fn unload_deletes_persisted_package() -> TestResult {
    let v1 = reload_package(&compile_reload_beam(1)?, "run")?;
    let v2 = reload_package(&compile_reload_beam(2)?, "run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![]).await?;
    engine.load_package(v1.clone()).await?;
    engine.load_package(v2.clone()).await?;
    // v2 is route-active, v1 is unpinned: unload v1.
    engine
        .unload_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await?;
    let persisted = store.list_packages().await?;
    assert_eq!(
        persisted.len(),
        1,
        "unload must delete the persisted artifact: {persisted:#?}"
    );
    assert_eq!(persisted[0].content_hash, v2.content_hash().to_string());
    engine.shutdown()?;

    let recovered = engine_with(&store, vec![]).await?;
    let versions = recovered.list_workflow_versions()?;
    assert_eq!(
        versions.len(),
        1,
        "only the surviving version may reload: {versions:#?}"
    );
    assert_eq!(&versions[0].content_hash, v2.content_hash());
    assert!(versions[0].route_active);
    recovered.shutdown()?;
    Ok(())
}

// --- the file-based startup path stays a trusted, non-persisted source -------

/// `--workflow-package`-style startup sources are NOT persisted: they are
/// operator-supplied every boot. A restart without the source comes up with
/// an empty catalog (and no phantom persisted rows).
#[tokio::test]
async fn file_sourced_packages_are_not_persisted() -> TestResult {
    let v1 = reload_package(&compile_reload_beam(1)?, "run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![v1]).await?;
    assert!(
        store.list_packages().await?.is_empty(),
        "builder-sourced packages must not be persisted"
    );
    engine.shutdown()?;

    let recovered = engine_with(&store, vec![]).await?;
    assert!(
        recovered.list_workflow_versions()?.is_empty(),
        "nothing was deployed at runtime, so nothing reloads"
    );
    recovered.shutdown()?;
    Ok(())
}

/// A startup file source named explicitly at restart wins the route over a
/// reloaded persisted deploy of the same type, while the persisted version
/// still reloads for pinned runs.
#[tokio::test]
async fn startup_file_source_route_wins_over_reloaded_deploys() -> TestResult {
    let v1 = reload_package(&compile_reload_beam(1)?, "run")?;
    let v2 = reload_package(&compile_reload_beam(2)?, "run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![]).await?;
    engine.load_package(v1.clone()).await?;
    engine.shutdown()?;

    // Restart names v2 on the command line: the explicit operator intent at
    // boot re-points the route, but persisted v1 still reloads.
    let recovered = engine_with(&store, vec![v2.clone()]).await?;
    let versions = recovered.list_workflow_versions()?;
    assert_eq!(versions.len(), 2, "{versions:#?}");
    let routed = versions
        .iter()
        .find(|version| version.route_active)
        .ok_or("one version must be route-active")?;
    assert_eq!(
        &routed.content_hash,
        v2.content_hash(),
        "the explicit startup source must win the route"
    );
    let (workflow_id, run_id) = start(&recovered).await?;
    assert_eq!(result_int(&recovered, &workflow_id, &run_id).await?, 2);
    recovered.shutdown()?;
    Ok(())
}
