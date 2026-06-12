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

use std::sync::Arc;

use aion_store::{EventStore, InMemoryStore};

use reload_fixture::{
    RELOAD_MODULE, compile_reload_beam, engine_with, input, recorded_version, reload_package,
    result_int, start, version_of,
};

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
    recovered.signal(&new_id, &new_run, "step", input()?).await?;
    recovered
        .signal(&new_id, &new_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&recovered, &new_id, &new_run).await?, 1);
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
