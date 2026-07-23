//! Task #119 — a PARKED durable-timer workflow must resume on a survivor that
//! ADOPTS its shard live, exactly as it would on a cold restart.
//!
//! The fan-out / in-flight path (a workflow with only `WorkflowStarted` durable)
//! is already proven by `ss5_failover_demo`. This gate targets the harder case:
//! a workflow PARKED on a durable timer (`TimerStarted` recorded, no terminal,
//! the process gone). Cold-restart recovery re-arms such a timer
//! (`recover_timers_on_startup` after `recover_active_workflows_on_startup`).
//! Live shard adoption must do the SAME through `Engine::adopt_shards`.
//!
//! ## Deterministic single-process reproduction
//!
//! A multi-node loopback cluster (the `ss5` harness) is slow and racy for this.
//! Instead this drives the EXACT engine adoption seam in one process over a
//! single-node multi-shard `HaematiteStore`:
//!
//!   1. Build engine A owning ALL shards; start the `sleep_query` sleeper with a
//!      LONG sleep so it parks on a durable timer; capture its shard; shut A down
//!      (its parked timer stays durable).
//!   2. Build engine B (the SURVIVOR) owning every shard EXCEPT the sleeper's, so
//!      its BOOT recovery never sees the parked workflow.
//!   3. `B.adopt_shards(&[sleeper_shard])` — the production failover entry point.
//!      This must re-resident the parked workflow AND re-arm its durable timer.
//!   4. The re-armed timer must FIRE and the workflow must complete with "slept"
//!      exactly once on B.
//!
//! The sleep is short enough that the re-armed wheel fires it promptly; the gate
//! is whether adoption re-arms it AT ALL, not the wall-clock latency.

#[path = "test_support/gleam.rs"]
mod gleam_test_support;

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::{Engine, EngineBuilder};
use aion_core::{Event, Payload, RunId, WorkflowId, WorkflowStatus};
use aion_store::EventStore;
use aion_store_haematite::HaematiteStore;
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SHARD_COUNT: usize = 4;
/// Long enough that the sleeper is reliably still parked when A shuts down, short
/// enough that the re-armed wheel on B fires it within the test deadline.
const SLEEP_MS: u64 = 1_500;
const COMPLETE_DEADLINE: Duration = Duration::from_secs(30);

fn package() -> Result<aion_package::Package, Box<dyn std::error::Error>> {
    example_build::built_package("crates/aion/tests/fixtures/sleep_query", "sleep_query")
}

async fn build_engine(
    store: &Arc<dyn EventStore>,
    owned_shards: Vec<usize>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .owned_shards(owned_shards)
        .load_workflows(package()?)
        .build()
        .await?)
}

async fn start_sleeper(
    engine: &Engine,
    sleep_ms: u64,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let input = Payload::from_json(&json!({ "sleep_ms": sleep_ms }))?;
    let handle = engine
        .start_workflow(
            "sleep_query",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

/// Spin until the workflow's history contains `TimerStarted` (it has parked on
/// the durable sleep) but NOT yet `TimerFired`.
async fn wait_until_parked(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + COMPLETE_DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        let started = history
            .iter()
            .any(|event| matches!(event, Event::TimerStarted { .. }));
        let fired = history
            .iter()
            .any(|event| matches!(event, Event::TimerFired { .. }));
        if started && !fired {
            return Ok(history);
        }
        if Instant::now() >= deadline {
            return Err(
                format!("workflow never reached the parked-on-timer state: {history:#?}").into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parked_timer_workflow_resumes_on_shard_adoption() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let concrete = Arc::new(HaematiteStore::create_with_shard_count(
        dir.path().join("db"),
        SHARD_COUNT,
    )?);
    let store: Arc<dyn EventStore> = Arc::clone(&concrete) as Arc<dyn EventStore>;

    // --- Epoch A: own all shards, start + park the sleeper, then "crash". ---
    let engine_a = build_engine(&store, (0..SHARD_COUNT).collect()).await?;
    let (workflow_id, run_id) = start_sleeper(&engine_a, SLEEP_MS).await?;
    wait_until_parked(&store, &workflow_id).await?;
    let sleeper_shard = concrete.shard_for_workflow(&workflow_id);
    engine_a.shutdown()?;

    // The parked timer is durable, with no terminal yet.
    let parked = store.read_history(&workflow_id).await?;
    assert!(
        parked
            .iter()
            .any(|event| matches!(event, Event::TimerStarted { .. })),
        "the durable timer must be recorded before the failover: {parked:#?}"
    );
    assert!(
        !parked
            .iter()
            .any(|event| matches!(event, Event::TimerFired { .. })),
        "the timer must NOT have fired before the failover: {parked:#?}"
    );

    // --- Epoch B: the SURVIVOR boots owning every shard EXCEPT the sleeper's, so
    //     its boot recovery never re-arms the parked timer. ---
    let survivor_shards: Vec<usize> = (0..SHARD_COUNT).filter(|&s| s != sleeper_shard).collect();
    let engine_b = build_engine(&store, survivor_shards).await?;
    assert!(
        engine_b.registry().get(&workflow_id, &run_id)?.is_none(),
        "the parked workflow is out of the survivor's boot scope; it must NOT be resident yet"
    );

    // --- Failover: the survivor ADOPTS the sleeper's shard live. ---
    engine_b.adopt_shards(&[sleeper_shard]).await?;

    // The adopted parked workflow must re-arm its timer, fire it, and complete.
    let result = tokio::time::timeout(COMPLETE_DEADLINE, engine_b.result(&workflow_id, &run_id))
        .await
        .map_err(|_| {
            "adopted parked workflow never completed: its durable timer was not re-armed on adoption"
        })??;
    let payload = result.map_err(|error| format!("adopted workflow failed: {error:?}"))?;
    let output: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(output, json!("slept"));

    // Exactly-once: the timer fired once, the workflow completed once, and the
    // pre-failover history is a byte-identical prefix of the final history.
    let final_history = store.read_history(&workflow_id).await?;
    let fired = final_history
        .iter()
        .filter(|event| matches!(event, Event::TimerFired { .. }))
        .count();
    assert_eq!(
        fired, 1,
        "the durable timer must fire exactly once: {final_history:#?}"
    );
    let completed = final_history
        .iter()
        .filter(|event| matches!(event, Event::WorkflowCompleted { .. }))
        .count();
    assert_eq!(
        completed, 1,
        "the workflow must complete exactly once: {final_history:#?}"
    );
    assert_eq!(
        aion_core::status_from_events(&final_history),
        WorkflowStatus::Completed
    );
    assert_eq!(
        &final_history[..parked.len()],
        parked.as_slice(),
        "the re-armed resume must extend, never rewrite, the recorded history"
    );

    engine_b.shutdown()?;
    Ok(())
}
