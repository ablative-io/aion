//! Batch-orchestrator example e2e: live queries through the production
//! Gleam SDK pump while the parent is parked in `child.await`, plus a pinned
//! upstream-beamr defect on the child-terminal decode path.
//!
//! Both example archives are rebuilt from the committed example source on
//! every run — see `common/example_build.rs` for why this gate must never
//! skip. The historical `beamr_query_reentry_fixed` cargo feature (an
//! off-by-default compile gate, i.e. a silent skip) is gone: these tests now
//! always run and assert the exact current behavior.
//!
//! # BEAMR-UPSTREAM PIN: `byte_size`/`binary_part` badarg on refc binaries
//!
//! On beamr 0.5.0, `erlang:byte_size/1` and `erlang:binary_part/3` only
//! accept inline heap binaries (`Binary::new`); any binary over beamr's
//! 64-byte `REFC_BINARY_THRESHOLD` is a `ProcBin` and raises badarg
//! (`crates/beamr/src/native/gate3_bifs/mod.rs` `binary_size` — standalone
//! 20-line Erlang repro, no aion involved). The batch parent's child-terminal
//! envelope (`ok:{...item json...}`) is ~100 bytes, so the SDK's
//! `string.starts_with` decode in `aion/child.decode_child_result` dies with
//! `VM execution error: bad argument` at the FIRST child completion — with
//! or without queries. Everything the suspension protocol owns works and is
//! asserted here: queries are answered while the parent is parked in
//! `child.await`, re-enter the await cleanly, and append no history (the
//! small-payload green path completes end-to-end in
//! `tests/child_await_query_e2e.rs`).
//!
//! The pinned assertions assert the defect's exact signature ON PURPOSE:
//! the moment a beamr bump fixes `byte_size` on refc binaries, they FAIL
//! loudly — restore the full `finish_and_assert_summary` completion
//! assertions from git history to flip the suite to end-to-end completion.

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

/// BEAMR-UPSTREAM PIN (see module docs): assert the run fails at the first
/// child-terminal decode with the exact upstream signature. When a beamr
/// bump fixes `byte_size`/`binary_part` on refc binaries this assertion
/// fails — restore the full `finish_and_assert_summary` completion
/// assertions (summary counts 4/3/1) from git history.
async fn assert_pinned_upstream_decode_failure(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> TestResult {
    let result = engine.result(workflow_id, run_id).await?;
    let history = store.read_history(workflow_id).await?;
    match result {
        Err(error) if error.message.contains("VM execution error: bad argument") => Ok(()),
        Err(error) => Err(format!(
            "expected the pinned upstream byte_size-on-refc-binary failure, got a \
             different workflow error: {error:?}\nhistory: {history:#?}"
        )
        .into()),
        Ok(payload) => {
            // The upstream defect is FIXED: fail loudly so this suite is
            // flipped to `finish_and_assert_summary` (full e2e completion).
            let summary: serde_json::Value = serde_json::from_slice(payload.bytes())?;
            Err(format!(
                "the batch orchestrator completed ({summary}) — beamr's \
                 byte_size-on-refc-binary defect is fixed; replace \
                 assert_pinned_upstream_decode_failure with \
                 finish_and_assert_summary in tests/example_query_reentry.rs"
            )
            .into())
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn the_pinned_upstream_decode_defect_fails_the_never_queried_control() -> TestResult {
    let packages = example_packages()?;
    let gates = GateBoard::new();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, &gates, packages).await?;
    let (workflow_id, run_id) = start_batch(&engine).await?;

    for id in ITEM_IDS {
        gates.release(id);
    }
    assert_pinned_upstream_decode_failure(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn live_query_while_parked_in_sdk_child_await() -> TestResult {
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

    // Resume: the first released child completes and its >64-byte terminal
    // envelope hits the pinned upstream decode defect (module docs). Once
    // beamr fixes it, restore the release-one-by-one query loop and
    // `finish_and_assert_summary` from git history.
    for id in ITEM_IDS {
        gates.release(id);
    }
    assert_pinned_upstream_decode_failure(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}
