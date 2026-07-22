//! Live package reload end-to-end: load into a running engine, latest-wins
//! routing for new starts, idempotent re-load, listing/re-pointing, unload
//! safety checks, and shutdown interaction (#62 Waves 2/3, brief §4 tests
//! 1, 2, 4, 8, 9, 10).
//!
//! Two distinguishable versions of one workflow type are compiled at test
//! time with `erlc` (same precedent as the engine builder tests): version N
//! completes with the integer N, and `park/1` blocks in `receive` until any
//! message (a signal marker) releases it, still completing with N.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, EngineError, RuntimeHandle, SignalRouter};
use aion_core::{Event, PackageVersion, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ContentHash, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RELOAD_MODULE: &str = "aion_reload_fixture";

/// Compiles the reload fixture returning `version` from both entrypoints.
fn compile_reload_beam(version: u32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let temp_dir = std::env::temp_dir().join(format!("aion-reload-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;
    let source_path = temp_dir.join(format!("{RELOAD_MODULE}.erl"));
    let beam_path = temp_dir.join(format!("{RELOAD_MODULE}.beam"));
    std::fs::write(
        &source_path,
        format!(
            "-module({RELOAD_MODULE}).\n\
             -export([run/1, park/1]).\n\
             run(_Input) -> {version}.\n\
             park(_Input) -> receive _Any -> {version} end.\n"
        ),
    )?;
    let status = Command::new("erlc")
        .arg("-o")
        .arg(&temp_dir)
        .arg(&source_path)
        .status()?;
    if !status.success() {
        let cleanup = std::fs::remove_dir_all(&temp_dir);
        drop(cleanup);
        return Err(format!("erlc failed with status {status}").into());
    }
    let bytes = std::fs::read(beam_path)?;
    std::fs::remove_dir_all(temp_dir)?;
    Ok(bytes)
}

fn reload_package(
    beam: &[u8],
    entry_function: &str,
) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(RELOAD_MODULE, beam.to_vec())])?;
    let manifest = Manifest {
        entry_module: RELOAD_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Some(Duration::from_secs(30)),
        activities: vec![],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// `(v1 package, v2 package)` with the given entry function.
fn two_versions(entry: &str) -> Result<(Package, Package), Box<dyn std::error::Error>> {
    let v1 = reload_package(&compile_reload_beam(1)?, entry)?;
    let v2 = reload_package(&compile_reload_beam(2)?, entry)?;
    assert_ne!(v1.content_hash(), v2.content_hash());
    Ok((v1, v2))
}

async fn engine_with(
    store: &Arc<dyn EventStore>,
    packages: Vec<Package>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    let mut builder = EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        });
    for package in packages {
        builder = builder.load_workflows(package);
    }
    Ok(builder.build().await?)
}

fn input() -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "reload": true }))
}

async fn start(engine: &Engine) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            RELOAD_MODULE,
            input()?,
            HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

async fn result_int(
    engine: &Engine,
    id: &WorkflowId,
    run: &RunId,
) -> Result<i64, Box<dyn std::error::Error>> {
    let payload = engine
        .result(id, run)
        .await?
        .map_err(|error| format!("workflow failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    value
        .as_i64()
        .ok_or_else(|| format!("expected integer result, got {value}").into())
}

fn recorded_version(
    history: &[Event],
    run_id: &RunId,
) -> Result<PackageVersion, Box<dyn std::error::Error>> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted {
                run_id: started_run,
                package_version,
                ..
            } if started_run == run_id => Some(package_version.clone()),
            _ => None,
        })
        .ok_or_else(|| "run has no WorkflowStarted".into())
}

fn version_of(package: &Package) -> PackageVersion {
    PackageVersion::new(package.content_hash().to_string())
}

// --- brief §4 test 1: load into a running engine + latest-wins ---------------

#[tokio::test]
async fn load_into_running_engine_routes_new_starts_while_old_run_finishes_on_v1() -> TestResult {
    let (v1, v2) = two_versions("park")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(&store, vec![v1.clone()]).await?;

    // Park a v1 instance, then load v2 into the RUNNING engine.
    let (parked_id, parked_run) = start(&engine).await?;
    let loaded = engine.load_package(v2.clone()).await?;
    assert_eq!(loaded.record.version(), v2.content_hash());
    assert!(loaded.freshly_loaded, "first v2 load must be fresh");
    assert!(loaded.route_changed, "first v2 load must take the route");

    // New starts route to v2 and complete with v2 behavior.
    let (new_id, new_run) = start(&engine).await?;
    engine
        .signal(&new_id, &new_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&engine, &new_id, &new_run).await?, 2);

    // The parked v1 instance, when released, completes with v1 behavior.
    engine
        .signal(&parked_id, &parked_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&engine, &parked_id, &parked_run).await?, 1);

    // Histories carry the respective recorded package versions.
    let parked_history = store.read_history(&parked_id).await?;
    assert_eq!(
        recorded_version(&parked_history, &parked_run)?,
        version_of(&v1)
    );
    let new_history = store.read_history(&new_id).await?;
    assert_eq!(recorded_version(&new_history, &new_run)?, version_of(&v2));

    engine.shutdown()?;
    Ok(())
}

// --- brief §4 test 2: route-flip atomicity under fire -------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn route_flip_under_concurrent_starts_is_atomic() -> TestResult {
    let (v1, v2) = two_versions("run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = Arc::new(engine_with(&store, vec![v1.clone()]).await?);

    // Task A: loop starts; task B: load v2 mid-stream.
    let starter = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move {
            let mut runs = Vec::new();
            for _ in 0..40 {
                let handle = engine
                    .start_workflow(
                        RELOAD_MODULE,
                        input()?,
                        HashMap::new(),
                        String::from("default"),
                    )
                    .await?;
                runs.push((handle.workflow_id().clone(), handle.run_id().clone()));
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(runs)
        })
    };
    tokio::time::sleep(Duration::from_millis(25)).await;
    engine.load_package(v2.clone()).await?;
    // Every start initiated after load_package returned must be v2.
    let (after_id, after_run) = start(&engine).await?;

    let runs = starter.await?.map_err(|error| error.to_string())?;
    let mut seen = Vec::new();
    for (id, run) in &runs {
        // Every start succeeds and completes entirely on one version.
        let value = result_int(&engine, id, run).await?;
        assert!(value == 1 || value == 2, "torn version result: {value}");
        let history = store.read_history(id).await?;
        let recorded = recorded_version(&history, run)?;
        let expected = if value == 1 {
            version_of(&v1)
        } else {
            version_of(&v2)
        };
        assert_eq!(
            recorded, expected,
            "recorded version must match the executed version"
        );
        seen.push(value);
    }
    // Monotone flip: once a start lands on v2, no later start is v1.
    let first_v2 = seen.iter().position(|value| *value == 2);
    if let Some(first_v2) = first_v2 {
        assert!(
            seen[first_v2..].iter().all(|value| *value == 2),
            "route flip must be monotone: {seen:?}"
        );
    }
    assert_eq!(result_int(&engine, &after_id, &after_run).await?, 2);

    engine.shutdown()?;
    Ok(())
}

// --- brief §4 test 4: idempotent re-load + re-route of a rolled-back hash ----

#[tokio::test]
async fn idempotent_reload_registers_nothing_and_re_routes_rolled_back_versions() -> TestResult {
    let (v1, v2) = two_versions("run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(&store, vec![v1.clone()]).await?;

    let first = engine.load_package(v1.clone()).await?;
    let second = engine.load_package(v1.clone()).await?;
    assert_eq!(
        first.record, second.record,
        "re-load must return the identical record"
    );
    assert!(!second.freshly_loaded, "re-load must report idempotence");
    assert!(
        !second.route_changed,
        "re-loading the route-active hash is a full no-op"
    );
    assert_eq!(engine.list_workflow_versions()?.len(), 1);
    assert!(
        engine
            .runtime()
            .has_registered_module(&v1.deployed_entry_module())
    );

    // Load v2 (route moves), then re-deploy the v1 archive: the
    // previously rolled-back hash must take the route again.
    engine.load_package(v2.clone()).await?;
    let (id, run) = start(&engine).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 2);

    let re_deployed = engine.load_package(v1.clone()).await?;
    assert_eq!(re_deployed.record, first.record);
    assert!(!re_deployed.freshly_loaded);
    assert!(
        re_deployed.route_changed,
        "re-deploying a rolled-back hash must re-point the route"
    );
    assert_eq!(engine.list_workflow_versions()?.len(), 2);
    let (id, run) = start(&engine).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 1);

    engine.shutdown()?;
    Ok(())
}

// --- brief §4 test 8: listing / re-pointing ----------------------------------

#[tokio::test]
async fn listing_shows_route_flags_and_route_workflow_version_re_points() -> TestResult {
    let (v1, v2) = two_versions("run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(&store, vec![v1.clone(), v2.clone()]).await?;

    let versions = engine.list_workflow_versions()?;
    assert_eq!(versions.len(), 2);
    let route_active: Vec<&ContentHash> = versions
        .iter()
        .filter(|info| info.route_active)
        .map(|info| &info.content_hash)
        .collect();
    assert_eq!(
        route_active,
        vec![v2.content_hash()],
        "the route must point at the last source loaded"
    );

    // Listing types are serde-ready (D3).
    let serialized = serde_json::to_string(&versions)?;
    assert!(serialized.contains(&v1.content_hash().to_string()));

    engine
        .route_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await?;
    let versions = engine.list_workflow_versions()?;
    let route_active: Vec<&ContentHash> = versions
        .iter()
        .filter(|info| info.route_active)
        .map(|info| &info.content_hash)
        .collect();
    assert_eq!(route_active, vec![v1.content_hash()]);
    let (id, run) = start(&engine).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 1);

    let unknown = ContentHash::from_bytes([9; 32]);
    let result = engine.route_workflow_version(RELOAD_MODULE, &unknown).await;
    assert!(
        matches!(&result, Err(EngineError::UnknownVersion { workflow_type, version, loaded })
            if workflow_type == RELOAD_MODULE
                && version == &unknown
                && loaded.contains(&v1.content_hash().to_string())),
        "routing to a never-loaded hash must fail typed: {result:?}"
    );

    engine.shutdown()?;
    Ok(())
}

// --- brief §4 test 9: unload refusals and success -----------------------------

#[tokio::test]
async fn unload_refuses_pinned_versions_and_succeeds_when_free() -> TestResult {
    let (v1, v2) = two_versions("park")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(&store, vec![v1.clone()]).await?;

    // A live run pinned to v1, then v2 takes the route.
    let (parked_id, parked_run) = start(&engine).await?;
    engine.load_package(v2.clone()).await?;

    // Refuse while route-active.
    let result = engine
        .unload_workflow_version(RELOAD_MODULE, v2.content_hash())
        .await;
    assert!(
        matches!(&result, Err(EngineError::RouteActive { workflow_type, version })
            if workflow_type == RELOAD_MODULE && version == v2.content_hash()),
        "unloading the routed version must be refused: {result:?}"
    );

    // Refuse while a resident run pins it.
    let result = engine
        .unload_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await;
    assert!(
        matches!(&result, Err(EngineError::VersionPinned {
            pinned_by: aion::PinHolder::LiveRun { workflow_id, .. },
            ..
        }) if workflow_id == &parked_id),
        "unload must name the live run pinning the version: {result:?}"
    );
    // The refusal restored the catalog: v1 stays loaded and routable.
    assert_eq!(engine.list_workflow_versions()?.len(), 2);

    // Release the v1 run; once terminal it pins nothing.
    engine
        .signal(&parked_id, &parked_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&engine, &parked_id, &parked_run).await?, 1);

    // Refuse while a recoverable (active-in-store) instance pins it: an
    // active history with no registry handle, pinned to v1.
    let ghost_id = WorkflowId::new_v4();
    let ghost_run = RunId::new_v4();
    let mut recorder = aion::durability::Recorder::new(ghost_id.clone(), Arc::clone(&store));
    recorder
        .record_workflow_started(
            chrono::Utc::now(),
            aion::durability::WorkflowStartRecord {
                workflow_type: RELOAD_MODULE.to_owned(),
                input: input()?,
                run_id: ghost_run.clone(),
                parent_run_id: None,
                package_version: version_of(&v1),
            },
        )
        .await?;
    let result = engine
        .unload_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await;
    assert!(
        matches!(&result, Err(EngineError::VersionPinned {
            pinned_by: aion::PinHolder::RecoverableRun { workflow_id },
            ..
        }) if workflow_id == &ghost_id),
        "unload must name the recoverable run pinning the version: {result:?}"
    );

    // Close the ghost run; v1 is now free and unloads.
    recorder
        .record_workflow_completed(chrono::Utc::now(), input()?)
        .await?;
    engine
        .unload_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await?;
    assert!(
        !engine
            .runtime()
            .has_registered_module(&v1.deployed_entry_module()),
        "unloaded modules must be unregistered from the runtime"
    );
    let result = engine
        .route_workflow_version(RELOAD_MODULE, v1.content_hash())
        .await;
    assert!(
        matches!(&result, Err(EngineError::UnknownVersion { .. })),
        "routing to an unloaded version must fail typed: {result:?}"
    );

    // New starts still route to v2.
    let (id, run) = start(&engine).await?;
    engine.signal(&id, &run, "release", input()?).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 2);

    engine.shutdown()?;
    Ok(())
}

// --- brief §4 test 10: shutdown interaction -----------------------------------

#[tokio::test]
async fn load_package_after_shutdown_is_refused() -> TestResult {
    let (v1, v2) = two_versions("run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(&store, vec![v1]).await?;

    engine.shutdown()?;
    let result = engine.load_package(v2).await;
    assert!(
        matches!(result, Err(EngineError::ShuttingDown)),
        "loads after shutdown must be refused: {result:?}"
    );
    Ok(())
}

// --- D10 rider: same-hash-different-manifest tripwire -------------------------

/// Two archives with identical beams but diverging manifests carry the same
/// content hash; re-loading the diverged archive into a running engine must
/// be refused typed with the catalog untouched, and the byte-identical
/// archive must keep re-loading idempotently.
#[tokio::test]
async fn same_hash_different_manifest_reload_is_refused() -> TestResult {
    let beam = compile_reload_beam(1)?;
    let original = reload_package(&beam, "run")?;
    let diverged = reload_package(&beam, "park")?;
    assert_eq!(
        original.content_hash(),
        diverged.content_hash(),
        "identical beams must carry the identical content hash"
    );

    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(&store, vec![original.clone()]).await?;

    let result = engine.load_package(diverged).await;
    assert!(
        matches!(&result, Err(EngineError::ManifestMismatch { workflow_type, version, .. })
            if workflow_type == RELOAD_MODULE && version == original.content_hash()),
        "diverged-manifest re-load must be refused typed: {result:?}"
    );

    // The refusal left the resident version fully serviceable.
    assert_eq!(engine.list_workflow_versions()?.len(), 1);
    let (id, run) = start(&engine).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 1);

    // The byte-identical archive still re-loads idempotently.
    let again = engine.load_package(original).await?;
    assert!(!again.freshly_loaded);
    assert!(!again.route_changed);

    engine.shutdown()?;
    Ok(())
}
