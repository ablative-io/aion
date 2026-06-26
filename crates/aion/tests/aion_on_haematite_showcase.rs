//! Showcase: a REAL Aion workflow executing on **haematite** as its durable
//! backend, single-node, surviving a full process-style restart.
//!
//! This is a tangible, runnable demonstration — not a unit assertion. It prints
//! an act-by-act narration of what is happening so a human can read the story
//! without stepping through code:
//!
//!   * ACT 1 — open a fresh [`HaematiteStore`] on a real on-disk directory and
//!     build a live Aion engine over it.
//!   * ACT 2 — run the committed `hello_world` Gleam workflow to completion
//!     through the real engine: it schedules an activity, the activity is
//!     dispatched and completed, and the workflow finishes. Every durable event
//!     of that lifecycle is written into haematite.
//!   * ACT 3 — drop the engine AND the store, simulating a process exit. The
//!     only thing that persists is the bytes haematite committed to disk.
//!   * ACT 4 — reopen a BRAND NEW [`HaematiteStore`] from the SAME directory and
//!     build a SECOND engine over it. Engine startup runs recovery against the
//!     reopened store.
//!   * ACT 5 — prove the completed workflow's state was recovered FROM haematite
//!     (read back from disk, not from any in-memory state of the first engine).
//!
//! ## Which level does this achieve? (per the brief)
//!
//! This is the **(a)** level: real workflow execution on haematite. The
//! `hello_world` workflow is genuine Gleam compiled to BEAM modules, loaded into
//! a real beamr runtime, started, and run to completion through the engine's NIF
//! seams and activity dispatcher. The durable history
//! (`WorkflowStarted` → `ActivityScheduled` → `ActivityStarted` →
//! `ActivityCompleted` → `WorkflowCompleted`) is recorded into
//! [`HaematiteStore`], which persists it on
//! a single-node [`haematite::Database`] on disk. There is no `InMemoryStore`
//! and no fabricated event stream anywhere in the durable path.
//!
//! ## What the restart actually proves
//!
//! The first engine and the first `HaematiteStore` handle are fully dropped
//! before the second store is opened. The second store is a fresh
//! `HaematiteStore::open(<same dir>)` — a cold reopen of the on-disk database.
//! The completed result the second engine returns is read by `Engine::result`,
//! which resolves a terminal outcome straight from the store's history. So the
//! greeting we print after restart could only have come from bytes haematite
//! wrote to disk during ACT 2 and read back during ACT 4–5.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p aion-rs --test aion_on_haematite_showcase -- --nocapture
//! ```

// The `hello_world` archive is rebuilt from the committed Gleam source on every
// run (see `common/example_build.rs`); this gate never skips on a missing CLI.
#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;

use aion::EngineBuilder;
use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion_core::{Event, Payload, WorkflowStatus};
use aion_store::EventStore;
use aion_store_haematite::HaematiteStore;
use serde_json::json;

/// The host-side activity the `hello_world` workflow dispatches. Identical to
/// the one in `hello_world_e2e` / `recovery_e2e`: it builds a greeting string.
struct GreetDispatcher;

impl ActivityDispatcher for GreetDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let name = request.name.as_str();
        let input = request.input.as_str();
        if name != "greet" {
            return Err(format!("terminal:unknown activity {name}"));
        }
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|e| format!("terminal:bad input: {e}"))?;
        let who = value["name"].as_str().unwrap_or("stranger");
        Ok(json!({ "greeting": format!("Hello, {who}! Welcome to Aion.") }).to_string())
    }
}

/// Render a durable history as a compact, human-readable timeline so the
/// narration shows EXACTLY what haematite is holding.
fn render_history(history: &[Event]) -> String {
    history
        .iter()
        .map(|event| {
            let kind = match event {
                Event::WorkflowStarted { .. } => "WorkflowStarted",
                Event::ActivityScheduled { .. } => "ActivityScheduled",
                Event::ActivityStarted { .. } => "ActivityStarted",
                Event::ActivityCompleted { .. } => "ActivityCompleted",
                Event::WorkflowCompleted { .. } => "WorkflowCompleted",
                other => return format!("    seq ? | {other:?}"),
            };
            format!("    seq {:>2} | {kind}", event.seq())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// This test is a deliberately linear, narrated story (act by act), so it runs
// long. The narration is the point — keeping it as one readable sequence is
// more valuable here than splitting it across helpers.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn aion_workflow_survives_restart_recovered_from_haematite()
-> Result<(), Box<dyn std::error::Error>> {
    // A real on-disk directory is the heart of the demo: this is where
    // haematite commits the workflow's durable state, and it is the ONLY thing
    // that crosses the restart boundary. A `TempDir` keeps it real (a genuine
    // filesystem path) while cleaning up when the test ends.
    let data_dir = tempfile::tempdir()?;
    let store_path = data_dir.path().join("aion-haematite-db");

    println!("\n================================================================");
    println!(" Aion on haematite — a real workflow, durable across a restart");
    println!("================================================================");
    println!(" durable backend : HaematiteStore (single-node haematite)");
    println!(" on-disk path    : {}", store_path.display());
    println!(" workflow        : hello_world (real Gleam → BEAM, run live)");

    // The committed Gleam example, compiled fresh and packaged into a real
    // `.aion` archive. This is genuine workflow code, not a hand-written event
    // stream.
    let package = example_build::built_package("examples/hello-world", "hello_world")?;

    // ----------------------------------------------------------------------
    // ACT 1 — open a fresh haematite store and build a live engine over it.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 1: open haematite + build the engine ---");
    let first_store = HaematiteStore::create(&store_path)?;
    println!("  haematite database CREATED at the path above.");
    // Type-erase the concrete store into the engine's `dyn EventStore` seam.
    // This is the entire wiring: HaematiteStore IS the engine's event store.
    let store: Arc<dyn EventStore> = Arc::new(first_store);
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .load_workflows(package.clone())
        .build()
        .await?;
    println!("  engine BUILT — its durable EventStore is haematite.");

    // ----------------------------------------------------------------------
    // ACT 2 — run the real workflow to completion. Every durable event lands
    // in haematite as it happens.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 2: run the hello_world workflow to completion ---");
    let input = Payload::from_json(&json!({ "name": "Ada" }))?;
    let handle = engine
        .start_workflow(
            "hello_world",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();
    println!("  started workflow id = {workflow_id}");
    println!("                run id = {run_id}");

    // Block until the workflow finishes. This drives the activity dispatch and
    // completion through the live BEAM runtime.
    let result = engine.result(&workflow_id, &run_id).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    println!("  workflow COMPLETED. result greeting = {greeting}");
    assert_eq!(greeting, json!("Hello, Ada! Welcome to Aion."));

    // Read the durable history straight back out of haematite and show it. This
    // is the full activity lifecycle, persisted on disk.
    let pre_restart_history = store.read_history(&workflow_id).await?;
    println!(
        "  durable history now in haematite ({} events):",
        pre_restart_history.len()
    );
    println!("{}", render_history(&pre_restart_history));
    assert_eq!(
        aion_core::status_from_events(&pre_restart_history),
        WorkflowStatus::Completed,
        "workflow should be Completed in haematite before restart"
    );
    // Sanity: this is the exact five-event lifecycle the brief calls for.
    assert_eq!(
        pre_restart_history.len(),
        5,
        "expected the full 5-event lifecycle"
    );
    assert!(matches!(
        pre_restart_history.first(),
        Some(Event::WorkflowStarted { .. })
    ));
    assert!(matches!(
        pre_restart_history.last(),
        Some(Event::WorkflowCompleted { .. })
    ));

    // ----------------------------------------------------------------------
    // ACT 3 — the "crash": drop the engine and EVERY handle to the store. After
    // this point nothing about the workflow lives in memory; the only record of
    // it is what haematite committed to `store_path`.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 3: simulate a restart (drop engine + store) ---");
    engine.shutdown()?;
    drop(store);
    println!("  engine shut down and the haematite handle dropped.");
    println!("  the ONLY surviving state is on disk at the path above.");

    // ----------------------------------------------------------------------
    // ACT 4 — cold reopen: a brand-new HaematiteStore opened from the SAME path,
    // and a SECOND engine built over it. Building runs startup recovery against
    // the reopened, on-disk database.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 4: reopen haematite from disk + rebuild the engine ---");
    let reopened_store: Arc<dyn EventStore> = Arc::new(HaematiteStore::open(&store_path)?);
    println!("  haematite database REOPENED from disk (fresh handle).");

    // Prove the bytes are really on disk BEFORE the engine touches them: read
    // the history directly from the freshly-opened store.
    let recovered_history = reopened_store.read_history(&workflow_id).await?;
    println!(
        "  history read straight from the reopened store ({} events):",
        recovered_history.len()
    );
    println!("{}", render_history(&recovered_history));
    assert_eq!(
        recovered_history, pre_restart_history,
        "history read after reopen must be byte-for-byte what we wrote before the restart"
    );

    let recovered_engine = EngineBuilder::new()
        .store_arc(Arc::clone(&reopened_store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .load_workflows(package)
        .build()
        .await?;
    println!("  second engine BUILT over the reopened haematite store (recovery ran).");

    // ----------------------------------------------------------------------
    // ACT 5 — prove the recovered state came from haematite. `Engine::result`
    // resolves a terminal outcome from the store's history first, so the
    // greeting below is reconstituted from the on-disk WorkflowCompleted event.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 5: prove the completed state was recovered from haematite ---");
    let recovered_result = recovered_engine.result(&workflow_id, &run_id).await?;
    let recovered_payload =
        recovered_result.map_err(|error| format!("recovered workflow failed: {error:?}"))?;
    let recovered_greeting: serde_json::Value = serde_json::from_slice(recovered_payload.bytes())?;
    println!("  result after restart  = {recovered_greeting}");
    assert_eq!(
        recovered_greeting,
        json!("Hello, Ada! Welcome to Aion."),
        "the recovered result must match the pre-restart result"
    );

    let post_restart_status = aion_core::status_from_events(&recovered_history);
    println!("  status after restart  = {post_restart_status:?}");
    assert_eq!(post_restart_status, WorkflowStatus::Completed);

    // Recovery must not have re-recorded the lifecycle: exactly one
    // WorkflowStarted, history unchanged from what we persisted.
    let final_history = reopened_store.read_history(&workflow_id).await?;
    assert_eq!(
        final_history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1,
        "restart must not duplicate the workflow start"
    );
    assert_eq!(
        final_history, pre_restart_history,
        "restart must not mutate the completed history"
    );

    println!("\n================================================================");
    println!(" RESULT: the workflow state survived a full restart, recovered");
    println!("         from haematite. The greeting above was read back from");
    println!("         the on-disk haematite database, not from memory.");
    println!("================================================================\n");

    recovered_engine.shutdown()?;
    Ok(())
}
