//! Batch-orchestrator example e2e: live queries through the production
//! Gleam SDK pump while the parent is parked in `child.await`, completing
//! end-to-end (>64-byte child-terminal envelopes included).
//!
//! Both example archives are rebuilt from the committed example source on
//! every run — see `common/example_build.rs` for why this gate must never
//! skip. The historical `beamr_query_reentry_fixed` cargo feature (an
//! off-by-default compile gate, i.e. a silent skip) is gone: these tests
//! always run and assert full completion.
//!
//! History note: on beamr 0.5.0 these tests pinned an upstream defect
//! (`byte_size`/`binary_part` badarg on refc binaries over the 64-byte
//! threshold, killing the run at the first >64-byte child terminal). beamr
//! 0.6.0 fixed it, the pin tripped loudly as designed, and the suite was
//! flipped back to the full `finish_and_assert_summary` completion
//! assertions.

#[path = "common/example_build.rs"]
mod example_build;

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aion::activity::bridge::ActivityDispatcher;
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, EngineError, QueryError, RuntimeHandle, SignalRouter};
use aion_core::{Payload, RunId, WorkflowId};
use aion_package::Package;
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const POLL_DEADLINE: Duration = Duration::from_secs(20);

/// Per-item gates for the `process-batch-item` activity dispatcher.
struct GateBoard {
    released: Mutex<HashSet<String>>,
    condvar: Condvar,
}

impl GateBoard {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            released: Mutex::new(HashSet::new()),
            condvar: Condvar::new(),
        })
    }

    fn release(&self, key: &str) {
        if let Ok(mut released) = self.released.lock() {
            released.insert(key.to_owned());
            self.condvar.notify_all();
        }
    }

    fn wait(&self, key: &str) -> Result<(), String> {
        let deadline = std::time::Instant::now() + POLL_DEADLINE;
        let mut released = self
            .released
            .lock()
            .map_err(|_| "gate board lock poisoned".to_owned())?;
        while !released.contains(key) {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| format!("gate {key} was never released"))?;
            let (guard, _) = self
                .condvar
                .wait_timeout(released, remaining)
                .map_err(|_| "gate board lock poisoned".to_owned())?;
            released = guard;
        }
        Ok(())
    }
}

/// Gated `process-batch-item` dispatcher mirroring the example worker's
/// deterministic contract: blocks until the item's id gate is released,
/// then succeeds, or fails terminally for ids/payloads containing `fail`.
struct GatedItemDispatcher {
    gates: Arc<GateBoard>,
}

impl ActivityDispatcher for GatedItemDispatcher {
    fn dispatch(
        &self,
        _namespace: &str,
        name: &str,
        input: &str,
        _config: &str,
        _attempt: u32,
    ) -> Result<String, String> {
        if name != "process-batch-item" {
            return Err(format!("terminal:unknown activity {name}"));
        }
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|e| format!("terminal:bad input: {e}"))?;
        let id = value["id"].as_str().unwrap_or_default().to_owned();
        let payload = value["payload"].as_str().unwrap_or_default().to_owned();
        self.gates.wait(&id).map_err(|e| format!("terminal:{e}"))?;
        if id.contains("fail") || payload.contains("fail") {
            return Err(format!("terminal:deterministic failure for item {id}"));
        }
        Ok(json!({
            "item_id": id,
            "processed_payload": format!("processed:{payload}"),
            "detail": format!("processed item {id}")
        })
        .to_string())
    }
}

/// The README's canonical four-item batch: one payload intentionally fails.
const ITEM_IDS: [&str; 4] = ["item-1", "item-2", "item-3", "item-4"];

fn batch_input() -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({
        "items": [
            {"id": "item-1", "payload": "alpha"},
            {"id": "item-2", "payload": "beta"},
            {"id": "item-3", "payload": "please-fail"},
            {"id": "item-4", "payload": "delta"},
        ]
    }))
}

/// Both example archives, rebuilt from source (the parent spawns children
/// by workflow type, so the child archive must be loaded alongside).
fn example_packages() -> Result<(Package, Package), Box<dyn std::error::Error>> {
    Ok((
        example_build::built_package("examples/batch-orchestrator", "batch_orchestrator")?,
        example_build::built_package("examples/batch-orchestrator", "batch_orchestrator_item")?,
    ))
}

async fn engine_over(
    store: &Arc<dyn EventStore>,
    gates: &Arc<GateBoard>,
    packages: (Package, Package),
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .query_timeout(Duration::from_secs(5))
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::new(GatedItemDispatcher {
            gates: Arc::clone(gates),
        }))
        .load_workflows(packages.0)
        .load_workflows(packages.1)
        .build()
        .await?)
}

async fn start_batch(engine: &Engine) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            "batch_orchestrator",
            batch_input()?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

/// Query `batch_progress`, retrying while the workflow has not yet executed
/// its registration (registration is workflow code, racing the caller).
async fn progress_when_registered(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        match engine.query(workflow_id, run_id, "batch_progress").await {
            Err(EngineError::Query(QueryError::UnknownQuery(_)))
                if std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            outcome => {
                return Ok(serde_json::from_slice(outcome?.bytes())?);
            }
        }
    }
}

/// Await the batch result and assert the recorded summary (4 items, 3
/// succeeded, 1 deterministic failure).
async fn finish_and_assert_summary(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> TestResult {
    let result = engine.result(workflow_id, run_id).await?;
    let history = store.read_history(workflow_id).await?;
    let payload =
        result.map_err(|error| format!("batch failed: {error:?}\nhistory: {history:#?}"))?;
    let summary: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(summary["total_processed"], json!(4), "summary: {summary}");
    assert_eq!(summary["success_count"], json!(3), "summary: {summary}");
    assert_eq!(summary["failure_count"], json!(1), "summary: {summary}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn control_never_queried_batch_completes() -> TestResult {
    let packages = example_packages()?;
    let gates = GateBoard::new();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, &gates, packages).await?;
    let (workflow_id, run_id) = start_batch(&engine).await?;

    for id in ITEM_IDS {
        gates.release(id);
    }
    finish_and_assert_summary(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn live_query_while_parked_in_sdk_child_await_then_resume() -> TestResult {
    let packages = example_packages()?;
    let gates = GateBoard::new();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, &gates, packages).await?;
    let (workflow_id, run_id) = start_batch(&engine).await?;

    // Query while the parent is parked in the SDK pump-wrapped child.await
    // (all four item gates held, no child terminal exists anywhere).
    let progress = progress_when_registered(&engine, &workflow_id, &run_id).await?;
    assert_eq!(progress["total"], json!(4));
    assert_eq!(progress["pending"], json!(4));
    let parked = store.read_history(&workflow_id).await?;

    // Repeated queries each service the sentinel and re-enter the same
    // await; none of them may append history.
    for _ in 0..3 {
        let progress = progress_when_registered(&engine, &workflow_id, &run_id).await?;
        assert_eq!(progress["total"], json!(4));
    }
    assert_eq!(
        store.read_history(&workflow_id).await?,
        parked,
        "the query path must never append events"
    );

    // Resume: release the items one by one, querying between releases while
    // the parent is still parked on a later child (the final release races
    // workflow completion, so it is not followed by a query).
    for (index, id) in ITEM_IDS.iter().enumerate() {
        gates.release(id);
        if index + 1 < ITEM_IDS.len() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let progress = progress_when_registered(&engine, &workflow_id, &run_id).await?;
            assert_eq!(progress["total"], json!(4));
        }
    }

    finish_and_assert_summary(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}
