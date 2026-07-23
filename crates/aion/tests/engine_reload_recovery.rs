//! Multi-version recovery, replay, and crash-window sweep e2e for the #62
//! live-reload seam (brief §4 tests 5, 6, 7): every restart path resolves a
//! run's RECORDED package version, never a fresh "latest".

#[path = "test_support/gleam.rs"]
mod gleam_test_support;

#[path = "common/reload_fixture.rs"]
mod reload_fixture;

use std::sync::Arc;

use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use chrono::Utc;
use serde_json::json;

use aion_package::Package;
use reload_fixture::{
    RELOAD_MODULE, compile_reload_beam, engine_with, input, recorded_version, reload_package,
    result_int, start, version_of,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `(v1 package, v2 package)` with the given entry function.
fn two_versions(entry: &str) -> Result<(Package, Package), Box<dyn std::error::Error>> {
    let v1 = reload_package(&compile_reload_beam(1)?, entry)?;
    let v2 = reload_package(&compile_reload_beam(2)?, entry)?;
    assert_ne!(v1.content_hash(), v2.content_hash());
    Ok((v1, v2))
}

// --- brief §4 test 5: replay of an old-version run after a newer load --------

/// A v1 run with recorded progress survives an engine restart that loads
/// BOTH archives: recovery resolves the recorded v1, replays the recorded
/// signal deterministically, and the run completes with v1 behavior while
/// new starts route to v2. The pre-restart history is a byte-identical
/// prefix of the final history (#45 determinism-proof pattern).
#[tokio::test]
async fn old_version_run_replays_and_completes_on_v1_after_v2_loads() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let (v1, v2) = two_versions("gated")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    // Epoch 1: only v1 loaded; record progress (the `step` signal), then
    // "crash" (shutdown without completing).
    let engine = engine_with(&store, vec![v1.clone()]).await?;
    let (workflow_id, run_id) = start(&engine).await?;
    engine
        .signal(&workflow_id, &run_id, "step", input()?)
        .await?;
    let pre_restart = store.read_history(&workflow_id).await?;
    assert!(
        pre_restart
            .iter()
            .any(|event| matches!(event, Event::SignalReceived { name, .. } if name == "step")),
        "progress must be durable before the restart: {pre_restart:#?}"
    );
    engine.shutdown()?;

    // Epoch 2: BOTH versions loaded, route on v2 (last source wins).
    let recovered = engine_with(&store, vec![v1.clone(), v2.clone()]).await?;
    let handle = recovered
        .registry()
        .get(&workflow_id, &run_id)?
        .ok_or("v1 run must recover with both versions loaded")?;
    assert_eq!(
        handle.loaded_version(),
        v1.content_hash(),
        "recovery must resolve the RECORDED version, not the routed one"
    );

    // The recovered run completes on v1 once released.
    recovered
        .signal(&workflow_id, &run_id, "release", input()?)
        .await?;
    assert_eq!(result_int(&recovered, &workflow_id, &run_id).await?, 1);

    // Determinism proof: the pre-restart history is a byte-identical prefix.
    let final_history = store.read_history(&workflow_id).await?;
    assert!(
        final_history.len() > pre_restart.len(),
        "completion must append new events"
    );
    assert_eq!(
        &final_history[..pre_restart.len()],
        pre_restart.as_slice(),
        "replay must not rewrite recorded history"
    );
    assert_eq!(
        recorded_version(&final_history, &run_id)?,
        version_of(&v1),
        "the durable pin must survive the restart"
    );

    // New starts route to v2.
    let (new_id, new_run) = start(&recovered).await?;
    recovered
        .signal(&new_id, &new_run, "step", input()?)
        .await?;
    recovered
        .signal(&new_id, &new_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&recovered, &new_id, &new_run).await?, 2);
    let new_history = store.read_history(&new_id).await?;
    assert_eq!(recorded_version(&new_history, &new_run)?, version_of(&v2));

    recovered.shutdown()?;
    Ok(())
}

// --- brief §4 test 6: recovery with a missing pinned version -----------------

/// Restarting with only v2 loaded while a v1 run is active fails THAT
/// workflow's recovery (typed, naming the pinned hash, surfaced in the
/// engine log); v2 runs recover and the engine builds.
#[tokio::test]
async fn missing_pinned_version_fails_only_that_workflow_and_engine_builds() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let (v1, v2) = two_versions("gated")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let engine = engine_with(&store, vec![v1.clone()]).await?;
    let (v1_id, v1_run) = start(&engine).await?;
    engine.load_package(v2.clone()).await?;
    let (v2_id, v2_run) = start(&engine).await?;
    engine.shutdown()?;

    // Epoch 2: ONLY v2 loaded. The engine must still build.
    let recovered = engine_with(&store, vec![v2.clone()]).await?;

    // The v1 run failed recovery in isolation: no resident handle.
    assert!(
        recovered.registry().get(&v1_id, &v1_run)?.is_none(),
        "a run pinned to an unloaded version must not recover"
    );
    // The v2 run recovered on its pinned version and completes.
    let handle = recovered
        .registry()
        .get(&v2_id, &v2_run)?
        .ok_or("the v2 run must recover")?;
    assert_eq!(handle.loaded_version(), v2.content_hash());
    recovered.signal(&v2_id, &v2_run, "step", input()?).await?;
    recovered
        .signal(&v2_id, &v2_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&recovered, &v2_id, &v2_run).await?, 2);

    // The stranded v1 history is untouched, still pinned to v1.
    let stranded = store.read_history(&v1_id).await?;
    assert_eq!(recorded_version(&stranded, &v1_run)?, version_of(&v1));

    recovered.shutdown()?;
    Ok(())
}

// --- brief §4 test 7: crash-window sweeps respect recorded versions ----------

fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: Utc::now(),
        workflow_id: workflow_id.clone(),
    }
}

/// A parent crashed between recording `ChildWorkflowStarted{v1}` and the
/// child's start. With v2 routed, the startup sweep must start the child on
/// the RECORDED v1 — the crash path resolves identically to the crash-free
/// path.
#[tokio::test]
async fn child_spawn_recovery_sweep_starts_the_recorded_version() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let v1_park = reload_package(&compile_reload_beam(1)?, "park")?;
    let v2_park = reload_package(&compile_reload_beam(2)?, "park")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let parent_id = WorkflowId::new_v4();
    let parent_run = RunId::new_v4();
    let child_id = WorkflowId::new_v4();
    let child_input = Payload::from_json(&json!({ "child": true }))?;
    store
        .append(
            WriteToken::recorder(),
            &parent_id,
            &[
                Event::WorkflowStarted {
                    envelope: envelope(1, &parent_id),
                    workflow_type: RELOAD_MODULE.to_owned(),
                    input: input()?,
                    run_id: parent_run.clone(),
                    parent_run_id: None,
                    package_version: version_of(&v1_park),
                },
                Event::ChildWorkflowStarted {
                    envelope: envelope(2, &parent_id),
                    child_workflow_id: child_id.clone(),
                    workflow_type: RELOAD_MODULE.to_owned(),
                    input: child_input,
                    package_version: version_of(&v1_park),
                },
            ],
            0,
        )
        .await?;

    // Build with both versions; the route points at v2.
    let engine = engine_with(&store, vec![v1_park.clone(), v2_park.clone()]).await?;

    // The sweep started the child on the RECORDED v1, not the routed v2.
    let child_history = store.read_history(&child_id).await?;
    let child_run = child_history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .ok_or("sweep must start the recorded child")?;
    assert_eq!(
        recorded_version(&child_history, &child_run)?,
        version_of(&v1_park)
    );
    let child_handle = engine
        .registry()
        .get(&child_id, &child_run)?
        .ok_or("swept child must be resident")?;
    assert_eq!(child_handle.loaded_version(), v1_park.content_hash());

    // Released, the child completes with v1 behavior.
    engine
        .signal(&child_id, &child_run, "release", input()?)
        .await?;
    assert_eq!(result_int(&engine, &child_id, &child_run).await?, 1);

    engine.shutdown()?;
    Ok(())
}

/// A run crashed between `WorkflowContinuedAsNew` and the successor's
/// `WorkflowStarted`. Per the adopted D1, the sweep starts the successor on
/// the ROUTED (latest) version — the same rule as the live monitor path —
/// and records that version durably in the successor's `WorkflowStarted`.
#[tokio::test]
async fn continue_as_new_sweep_starts_the_routed_version_and_records_it() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let (v1, v2) = two_versions("run")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    let workflow_id = WorkflowId::new_v4();
    let first_run = RunId::new_v4();
    store
        .append(
            WriteToken::recorder(),
            &workflow_id,
            &[
                Event::WorkflowStarted {
                    envelope: envelope(1, &workflow_id),
                    workflow_type: RELOAD_MODULE.to_owned(),
                    input: input()?,
                    run_id: first_run.clone(),
                    parent_run_id: None,
                    package_version: version_of(&v1),
                },
                Event::WorkflowContinuedAsNew {
                    envelope: envelope(2, &workflow_id),
                    input: input()?,
                    workflow_type: None,
                    parent_run_id: first_run.clone(),
                },
            ],
            0,
        )
        .await?;

    let engine = engine_with(&store, vec![v1.clone(), v2.clone()]).await?;

    let history = store.read_history(&workflow_id).await?;
    let (successor_run, successor_version) = history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted {
                run_id,
                parent_run_id: Some(parent),
                package_version,
                ..
            } if parent == &first_run => Some((run_id.clone(), package_version.clone())),
            _ => None,
        })
        .ok_or("sweep must start the continue-as-new successor")?;
    assert_eq!(
        successor_version,
        version_of(&v2),
        "the successor takes the routed version at record time (D1)"
    );
    assert_eq!(
        result_int(&engine, &workflow_id, &successor_run).await?,
        2,
        "the successor must execute the routed v2 code"
    );

    engine.shutdown()?;
    Ok(())
}
