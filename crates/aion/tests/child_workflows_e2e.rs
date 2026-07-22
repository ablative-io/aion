//! Child-workflow correlation and replay end-to-end tests.
//!
//! These tests drive the production NIF path (`aion_flow_ffi:spawn_child/3`
//! and `await_child/1`) with real BEAM workflow fixtures over a shared
//! `InMemoryStore`, covering live spawn+await, crash-recovery replay after a
//! recorded child terminal, recovery mid-child, continue-as-new run scoping,
//! and correlation stability when asynchronous signal arrivals interleave
//! with child spawns. Child correlation is positional (the n-th spawn in a
//! run matches the n-th recorded `ChildWorkflowStarted` in that run's
//! segment), so restarts must never duplicate a child spawn and awaits must
//! resolve the recorded terminal for the same child workflow id.

use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{ContentType, Event, EventEnvelope, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use serde_json::json;

/// Real content-hash version of a single-module fixture package, in the
/// durable textual form recorded on start events. Synthesized histories
/// must pin the version the engine actually loads or recovery refuses them.
fn fixture_version(
    module: &str,
    beam: &[u8],
) -> Result<aion_core::PackageVersion, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(module, beam)])?;
    Ok(aion_core::PackageVersion::new(
        aion_package::content_hash(&beams).to_string(),
    ))
}

const PARENT_MODULE: &str = "aion_parent_fixture";
const CHILD_MODULE: &str = "aion_child_fixture";
const PARENT_BEAM: &[u8] = include_bytes!("fixtures/aion_parent_fixture.beam");
const PARENT_SOURCE: &[u8] = include_bytes!("fixtures/aion_parent_fixture.erl");
const CHILD_BEAM: &[u8] = include_bytes!("fixtures/aion_child_fixture.beam");
const CHILD_SOURCE: &[u8] = include_bytes!("fixtures/aion_child_fixture.erl");

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_package(
    module: &str,
    beam: &[u8],
    source: &[u8],
    entry_function: &str,
) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(module, beam)])?;
    let manifest = Manifest {
        entry_module: module.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Some(Duration::from_secs(30)),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive = PackageBuilder::with_source(manifest, beams, [(module, source.to_vec())])
        .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

fn parent_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
    fixture_package(PARENT_MODULE, PARENT_BEAM, PARENT_SOURCE, entry_function)
}

fn child_package() -> Result<Package, Box<dyn std::error::Error>> {
    fixture_package(CHILD_MODULE, CHILD_BEAM, CHILD_SOURCE, "complete")
}

async fn engine_over(
    store: &Arc<dyn EventStore>,
    parent_entry: &str,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .load_workflows(parent_package(parent_entry)?)
        .load_workflows(child_package()?)
        .build()
        .await?)
}

fn parent_input() -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "fixture": "input" }))?)
}

fn release_payload() -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "release": true }))?)
}

async fn wait_for_history<F>(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    description: &str,
    predicate: F,
) -> Result<Vec<Event>, Box<dyn std::error::Error>>
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("timed out waiting for {description}: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn child_started_ids(history: &[Event]) -> Vec<WorkflowId> {
    history
        .iter()
        .filter_map(|event| match event {
            Event::ChildWorkflowStarted {
                child_workflow_id, ..
            } => Some(child_workflow_id.clone()),
            _ => None,
        })
        .collect()
}

fn count_child_completed(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ChildWorkflowCompleted { .. }))
        .count()
}

fn result_json(payload: &Payload) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(payload.bytes())?)
}

#[tokio::test]
async fn child_workflow_runs_end_to_end_through_nif_path() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "child_round_trip").await?;

    let handle = engine
        .start_workflow(
            PARENT_MODULE,
            parent_input()?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine
        .result(handle.workflow_id(), handle.run_id())
        .await?
        .map_err(|error| format!("parent workflow failed: {error:?}"))?;

    let value = result_json(&result)?;
    let history = store.read_history(handle.workflow_id()).await?;
    let started = child_started_ids(&history);
    assert_eq!(
        started.len(),
        1,
        "exactly one child spawn must be recorded: {history:#?}"
    );
    assert_eq!(
        value,
        json!([started[0].to_string(), 42]),
        "parent must receive the child's terminal value through await_child"
    );
    assert_eq!(count_child_completed(&history), 1);

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn restart_after_child_completed_replays_recorded_child_without_respawn() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first = engine_over(&store, "child_then_signal").await?;
    let handle = first
        .start_workflow(
            PARENT_MODULE,
            parent_input()?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    // Wait until the child spawn and its terminal outcome are durable, then
    // "crash" with the parent still mid-run (gated on the release signal).
    let pre_restart = wait_for_history(&store, &workflow_id, "recorded child terminal", |events| {
        count_child_completed(events) == 1
    })
    .await?;
    let recorded_child = child_started_ids(&pre_restart);
    assert_eq!(recorded_child.len(), 1, "history: {pre_restart:#?}");
    first.shutdown()?;

    let recovered = engine_over(&store, "child_then_signal").await?;
    recovered
        .signal(&workflow_id, &run_id, "release", release_payload()?)
        .await?;
    let result = recovered
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;

    // Replay must return the recorded child id, not respawn a new child.
    assert_eq!(
        result_json(&result)?,
        json!([recorded_child[0].to_string(), 42])
    );
    let final_history = store.read_history(&workflow_id).await?;
    assert_eq!(
        child_started_ids(&final_history),
        recorded_child,
        "recovery replayed spawn must not append a duplicate ChildWorkflowStarted: {final_history:#?}"
    );
    assert_eq!(count_child_completed(&final_history), 1);

    recovered.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn restart_mid_child_resumes_awaiting_same_child() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    // Synthesize the crash point: the parent recorded the child spawn, the
    // child recorded its own start, and neither run reached a terminal.
    let parent_workflow_id = WorkflowId::new_v4();
    let parent_run_id = RunId::new_v4();
    let child_workflow_id = WorkflowId::new_v4();
    let child_run_id = RunId::new_v4();
    let recorded_at = chrono::Utc::now();
    store
        .append(
            WriteToken::recorder(),
            &parent_workflow_id,
            &[
                Event::WorkflowStarted {
                    envelope: EventEnvelope {
                        seq: 1,
                        recorded_at,
                        workflow_id: parent_workflow_id.clone(),
                    },
                    workflow_type: PARENT_MODULE.to_owned(),
                    input: parent_input()?,
                    run_id: parent_run_id.clone(),
                    parent_run_id: None,
                    package_version: fixture_version(PARENT_MODULE, PARENT_BEAM)?,
                },
                Event::ChildWorkflowStarted {
                    envelope: EventEnvelope {
                        seq: 2,
                        recorded_at,
                        workflow_id: parent_workflow_id.clone(),
                    },
                    child_workflow_id: child_workflow_id.clone(),
                    workflow_type: CHILD_MODULE.to_owned(),
                    input: Payload::new(ContentType::Json, br#""child-input""#.to_vec()),
                    package_version: fixture_version(CHILD_MODULE, CHILD_BEAM)?,
                },
            ],
            0,
        )
        .await?;
    store
        .append(
            WriteToken::recorder(),
            &child_workflow_id,
            &[Event::WorkflowStarted {
                envelope: EventEnvelope {
                    seq: 1,
                    recorded_at,
                    workflow_id: child_workflow_id.clone(),
                },
                workflow_type: CHILD_MODULE.to_owned(),
                input: Payload::new(ContentType::Json, br#""child-input""#.to_vec()),
                run_id: child_run_id,
                parent_run_id: None,
                package_version: fixture_version(CHILD_MODULE, CHILD_BEAM)?,
            }],
            0,
        )
        .await?;

    // Startup recovery re-spawns both actives. The parent's replayed spawn
    // must resolve to the recorded child id without spawning a second child,
    // and its await must bind to that same child's live completion.
    let engine = engine_over(&store, "child_then_signal").await?;
    wait_for_history(
        &store,
        &parent_workflow_id,
        "child terminal recorded into parent history",
        |events| count_child_completed(events) == 1,
    )
    .await?;
    engine
        .signal(
            &parent_workflow_id,
            &parent_run_id,
            "release",
            release_payload()?,
        )
        .await?;
    let result = engine
        .result(&parent_workflow_id, &parent_run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;

    assert_eq!(
        result_json(&result)?,
        json!([child_workflow_id.to_string(), 42]),
        "replay must keep awaiting the child id recorded before the crash"
    );
    let final_history = store.read_history(&parent_workflow_id).await?;
    assert_eq!(
        child_started_ids(&final_history),
        vec![child_workflow_id],
        "mid-child recovery must not respawn: {final_history:#?}"
    );

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn continue_as_new_scopes_child_correlation_to_each_run() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "child_then_signal").await?;
    let first_run = engine
        .start_workflow(
            PARENT_MODULE,
            parent_input()?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let workflow_id = first_run.workflow_id().clone();

    // Run N records its child, then rotates while gated on the release signal.
    wait_for_history(&store, &workflow_id, "run N child terminal", |events| {
        count_child_completed(events) == 1
    })
    .await?;
    let replacement = engine
        .continue_as_new(&workflow_id, first_run.run_id(), parent_input()?, None)
        .await?;
    let replacement_run_id = replacement.run_id().clone();

    // Run N+1 re-executes from scratch: it must spawn its own child rather
    // than match run N's recorded child events.
    let rotated = wait_for_history(&store, &workflow_id, "run N+1 child terminal", |events| {
        count_child_completed(events) == 2
    })
    .await?;
    let spawned = child_started_ids(&rotated);
    assert_eq!(spawned.len(), 2, "history: {rotated:#?}");
    assert_ne!(
        spawned[0], spawned[1],
        "the replacement run must spawn its own child"
    );
    engine.shutdown()?;

    // Restart: replay of run N+1 must match its own run-segment child events
    // only — no cross-run match, no third spawn.
    let recovered = engine_over(&store, "child_then_signal").await?;
    recovered
        .signal(
            &workflow_id,
            &replacement_run_id,
            "release",
            release_payload()?,
        )
        .await?;
    let result = recovered
        .result(&workflow_id, &replacement_run_id)
        .await?
        .map_err(|error| format!("recovered replacement run failed: {error:?}"))?;

    assert_eq!(
        result_json(&result)?,
        json!([spawned[1].to_string(), 42]),
        "run N+1 replay must resolve run N+1's child, not run N's"
    );
    let final_history = store.read_history(&workflow_id).await?;
    assert_eq!(
        child_started_ids(&final_history),
        spawned,
        "restart must not duplicate either run's child spawn: {final_history:#?}"
    );

    recovered.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn signal_between_spawns_preserves_child_identities_across_restart() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "two_children").await?;
    let handle = engine
        .start_workflow(
            PARENT_MODULE,
            parent_input()?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    // The fixture consumes a "mid" signal between its two spawns, so a
    // SignalReceived lands between the two recorded ChildWorkflowStarted
    // events.
    wait_for_history(&store, &workflow_id, "first child spawn", |events| {
        child_started_ids(events).len() == 1
    })
    .await?;
    engine
        .signal(&workflow_id, &run_id, "mid", release_payload()?)
        .await?;
    let pre_restart = wait_for_history(&store, &workflow_id, "both child terminals", |events| {
        count_child_completed(events) == 2
    })
    .await?;
    let recorded_children = child_started_ids(&pre_restart);
    assert_eq!(recorded_children.len(), 2, "history: {pre_restart:#?}");
    let mid_position = pre_restart
        .iter()
        .position(|event| matches!(event, Event::SignalReceived { name, .. } if name == "mid"))
        .ok_or("mid signal was not recorded")?;
    let second_spawn_position = pre_restart
        .iter()
        .rposition(|event| matches!(event, Event::ChildWorkflowStarted { .. }))
        .ok_or("second child spawn was not recorded")?;
    assert!(
        mid_position < second_spawn_position,
        "fixture contract: the mid signal must interleave the spawns: {pre_restart:#?}"
    );
    engine.shutdown()?;

    // Replay over the interleaved history must rebind both spawns to their
    // recorded identities: positional child ordinals ignore the interleaved
    // signal arrival.
    let recovered = engine_over(&store, "two_children").await?;
    recovered
        .signal(&workflow_id, &run_id, "release", release_payload()?)
        .await?;
    let result = recovered
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;

    assert_eq!(
        result_json(&result)?,
        json!([
            recorded_children[0].to_string(),
            recorded_children[1].to_string()
        ]),
        "each spawn must replay to its own recorded child identity"
    );
    let final_history = store.read_history(&workflow_id).await?;
    assert_eq!(
        child_started_ids(&final_history),
        recorded_children,
        "restart must not shift or duplicate child correlation: {final_history:#?}"
    );

    recovered.shutdown()?;
    Ok(())
}
