//! #222 — a reopened run's durable deadlines must come back with it.
//!
//! Found live (2026-07-04 lifecycle truth pass): cancelling a run records
//! `TimerCancelled` for its durable timers as part of the teardown, and reopen
//! never re-armed them. The reopened run stayed signal-responsive while its
//! clock was silently dead — an approval gate reopened after cancel would wait
//! forever instead of timing out. The prior reopen e2e used the timerless
//! `wait` fixture and asserted event shape, not forward progress, so the hole
//! was structurally invisible to it. These gates assert the FIRE.
//!
//! Instrument: the `sleep_query` Gleam fixture — parks on a durable timer and
//! completes with "slept" when it fires. Every test here derives its verdict
//! from that completion, not from history shape.

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::{Engine, EngineBuilder};
use aion_core::{Event, Payload, RunId, TimerCancelCause, WorkflowId};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const COMPLETE_DEADLINE: Duration = Duration::from_secs(30);

fn package() -> Result<aion_package::Package, Box<dyn std::error::Error>> {
    example_build::built_package("crates/aion/tests/fixtures/sleep_query", "sleep_query")
}

async fn build_engine(store: &Arc<dyn EventStore>) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
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
) -> Result<(), Box<dyn std::error::Error>> {
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
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                format!("workflow never reached the parked-on-timer state: {history:#?}").into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Await the run's terminal result and require the sleeper's success value.
async fn assert_completes_with_slept(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> TestResult {
    let outcome = tokio::time::timeout(COMPLETE_DEADLINE, engine.result(workflow_id, run_id))
        .await
        .map_err(|_| "reopened sleeper never completed: its re-armed timer did not fire")??;
    let payload =
        outcome.map_err(|error| format!("sleeper failed instead of completing: {error:?}"))?;
    let value: serde_json::Value = payload.to_json()?;
    assert_eq!(
        value,
        json!("slept"),
        "the reopened sleeper must complete through the timer path"
    );
    Ok(())
}

/// The cancel teardown must stamp its timer cancellations as `CancelTeardown`,
/// and reopen must append a fresh `TimerStarted` (the restart marker) at the
/// SAME original `fire_at` for each of them.
fn assert_teardown_then_rearm(history: &[Event]) -> TestResult {
    let teardown_fire_ats: Vec<_> = history
        .iter()
        .filter_map(|event| match event {
            Event::TimerCancelled {
                timer_id,
                cause: TimerCancelCause::CancelTeardown,
                ..
            } => Some(timer_id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !teardown_fire_ats.is_empty(),
        "cancel must stamp its timer teardown as CancelTeardown: {history:#?}"
    );

    let reopen_position = history
        .iter()
        .position(|event| matches!(event, Event::WorkflowReopened { .. }))
        .ok_or("history must contain WorkflowReopened")?;
    for timer_id in &teardown_fire_ats {
        let original = history
            .iter()
            .find_map(|event| match event {
                Event::TimerStarted {
                    timer_id: recorded,
                    fire_at,
                    ..
                } if recorded == timer_id => Some(*fire_at),
                _ => None,
            })
            .ok_or("teardown-cancelled timer must have an original TimerStarted")?;
        let rearmed = history[reopen_position..].iter().any(|event| {
            matches!(
                event,
                Event::TimerStarted { timer_id: recorded, fire_at, .. }
                    if recorded == timer_id && *fire_at == original
            )
        });
        assert!(
            rearmed,
            "reopen must re-record TimerStarted for {timer_id} at the ORIGINAL fire_at: {history:#?}"
        );
    }
    Ok(())
}

/// Cancel a parked sleeper BEFORE its deadline, reopen it, and require the
/// re-armed timer to fire and complete the run. This is the exact live repro
/// from the truth pass, as a permanent gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reopened_cancelled_sleeper_fires_its_deadline_and_completes() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = build_engine(&store).await?;

    // Long enough to reliably cancel while parked, short enough to fire fast
    // after reopen.
    let (workflow_id, run_id) = start_sleeper(&engine, 2_000).await?;
    wait_until_parked(&store, &workflow_id).await?;

    engine
        .cancel(&workflow_id, &run_id, "truth-pass repro")
        .await?;
    engine.reopen_workflow(&workflow_id, &run_id).await?;

    assert_teardown_then_rearm(&store.read_history(&workflow_id).await?)?;
    assert_completes_with_slept(&engine, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

/// Reopen AFTER the original deadline has already passed: the past-due timer
/// must fire immediately (timeout-branch semantics), not silently never.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reopen_after_the_deadline_passed_fires_immediately() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = build_engine(&store).await?;

    let (workflow_id, run_id) = start_sleeper(&engine, 400).await?;
    wait_until_parked(&store, &workflow_id).await?;
    engine
        .cancel(&workflow_id, &run_id, "cancel before deadline")
        .await?;

    // Let the original fire_at pass while the run sits cancelled.
    tokio::time::sleep(Duration::from_millis(600)).await;

    engine.reopen_workflow(&workflow_id, &run_id).await?;
    assert_completes_with_slept(&engine, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

/// Kill the engine after the reopen re-arm and rebuild on the same store: the
/// recovery replay must tolerate the re-arm marker
/// (`[TimerStarted, TimerCancelled(teardown), WorkflowReopened, TimerStarted]`)
/// and the run must still complete. This is the cursor-determinism gate for
/// the re-recorded `TimerStarted`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_after_rearm_replays_cleanly_and_completes() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine_a = build_engine(&store).await?;

    let (workflow_id, run_id) = start_sleeper(&engine_a, 2_500).await?;
    wait_until_parked(&store, &workflow_id).await?;
    engine_a
        .cancel(&workflow_id, &run_id, "cancel before restart")
        .await?;
    engine_a.reopen_workflow(&workflow_id, &run_id).await?;
    // "Crash" with the re-armed timer still pending.
    engine_a.shutdown()?;

    let engine_b = build_engine(&store).await?;
    assert_completes_with_slept(&engine_b, &workflow_id, &run_id).await?;
    engine_b.shutdown()?;
    Ok(())
}
