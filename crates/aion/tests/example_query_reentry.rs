//! Batch-orchestrator example e2e: live queries through the production
//! Gleam SDK pump while the parent is parked in `child.await`.
//!
//! # Runtime gate: `beamr_query_reentry_fixed`
//!
//! This module is compiled only with the `beamr_query_reentry_fixed` cargo
//! feature (off by default) because the example workflow cannot currently
//! run to completion on beamr 0.4.9. Empirical findings at HEAD
//! (2026-06-12, rebuilt archives, engine query pump in place):
//!
//! - The query pump protocol itself works end-to-end through the Gleam SDK:
//!   live `batch_progress` queries ARE answered while the parent is parked
//!   in the pump-wrapped `child.await`, repeated queries re-enter the same
//!   await cleanly, and the query path appends no history. The historical
//!   "invalid operand for instruction pointer" crash on await re-entry
//!   after a serviced query no longer reproduces.
//! - The run then dies — with or without queries (a never-queried control
//!   crashes identically) — when the parent decodes a child terminal
//!   payload through `gleam_json`/`gleam_stdlib` code paths that hit beamr
//!   0.4.9 VM gaps: `VM execution error: bad argument` on the success
//!   decode, and `undefined function erlang:integer_to_list/2` (raising
//!   `{invalid_byte, 0}` from `gleam_json_ffi:decode/1`) on the error
//!   decode. Both are upstream beamr stdlib/json defects being fixed on the
//!   separate beamr track, not query-re-entry bugs and not engine bugs.
//!
//! When the upstream fixes land and the beamr pin is bumped, build with
//! `--features beamr_query_reentry_fixed` and these tests prove the example
//! end-to-end; until then they are compile-checked by
//! `cargo check -p aion-rs --features beamr_query_reentry_fixed`.
#![cfg(feature = "beamr_query_reentry_fixed")]

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

/// Both example archives (the parent spawns children by workflow type, so
/// the child archive must be loaded alongside), or `None` when not built.
fn example_packages() -> Result<Option<(Package, Package)>, Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/batch-orchestrator");
    let parent_path = root.join("batch-orchestrator.aion");
    let item_path = root.join("batch-orchestrator-item.aion");
    if !parent_path.exists() || !item_path.exists() {
        eprintln!(
            "skipping: archives not built under {} (run `cargo run -p aion-cli -- package \
             examples/batch-orchestrator`)",
            root.display()
        );
        return Ok(None);
    }
    Ok(Some((
        Package::load_from_bytes(std::fs::read(&parent_path)?)?,
        Package::load_from_bytes(std::fs::read(&item_path)?)?,
    )))
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
    let Some(packages) = example_packages()? else {
        return Ok(());
    };
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
    let Some(packages) = example_packages()? else {
        return Ok(());
    };
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
