//! THE HEADLINE DEMO — a REAL Aion workflow whose durable state lives in a
//! 3-node **haematite** cluster, where the node that OWNS the workflow is KILLED
//! and the workflow's state SURVIVES on a survivor node that takes over.
//!
//! This is the culmination of the whole arc. The single-node showcase
//! (`aion_on_haematite_showcase.rs`) proved an Aion workflow survives a process
//! restart by recovering from haematite on disk. The B2 distributed gate
//! (`aion-store-haematite/tests/distributed_failover.rs`) proved a workflow's
//! durable history quorum-REPLICATES across a real haematite cluster and is
//! readable on a survivor after the owner dies. THIS test fuses them: a real
//! Aion ENGINE runs a real Gleam workflow to completion on node A's
//! `HaematiteStore`; that durable history replicates to the quorum; node A is
//! KILLED; a survivor (B) is promoted, merges the replicated history, and a
//! BRAND-NEW Aion engine is built over B's store — engine startup recovery runs
//! against the merged, replicated history — and it serves the SAME completed
//! result A produced. The state outlived the death of the node that owned it.
//!
//! ## The act-by-act story (printed for a human; run with `-- --nocapture`)
//!
//!   * ACT 1 — node A `acquire_shard_and_serve`s shard 0 over the cluster and
//!     builds a live Aion engine over A's distributed `HaematiteStore`.
//!   * ACT 2 — A runs the committed `hello_world` Gleam workflow to completion;
//!     every durable event is `replicate_append`ed to the quorum. We verify the
//!     survivor B holds the FULL history by reading B's store directly — the
//!     state really replicated, not just landed locally on A.
//!   * ACT 3 — KILL node A: stop its responder and exclude it from all future
//!     membership. A is, for every protocol purpose, DEAD. The only live copies
//!     of the workflow are on {B,C}.
//!   * ACT 4 — survivor B `acquire_shard_and_serve`s shard 0 over {B,C}: it
//!     fences dead A with a higher epoch and `become_live` union-merges the
//!     replicated history into B's tree, THEN a NEW Aion engine is built over
//!     B's `HaematiteStore` — building runs Aion startup recovery against the
//!     now-merged, replicated history.
//!   * ACT 5 — PROVE the workflow survived A's death: from B's engine, retrieve
//!     the workflow result and assert it matches what A produced. The result
//!     came from the replicated+merged haematite cluster, NOT from dead node A,
//!     and NOT from local disk on B alone (B never owned this shard until ACT 4).
//!
//! ## Why a single long-lived runtime + `block_on` (not `#[tokio::test]`)
//!
//! haematite's distribution coordinator (`acquire_shard_and_serve`,
//! `replicate_append`, …) refuses to run from a thread with an ENTERED tokio
//! runtime: it checks `Handle::try_current().is_ok()` and returns
//! `TransportBlockingFromAsync`. But the Aion engine's `build()` /
//! `start_workflow` / `result` are async and capture `Handle::current()` for the
//! background tasks they spawn (activity dispatch, process monitors, timers).
//!
//! The resolution: ONE long-lived [`tokio::runtime::Runtime`] is created up
//! front and kept alive for the whole test. EVERY async engine call is driven
//! through `runtime.block_on(..)`. While a `block_on` runs, the engine captures
//! THAT runtime's handle and spawns its tasks on THAT runtime's worker threads —
//! which outlive each individual `block_on`. The instant `block_on` returns, the
//! runtime context is no longer ENTERED on the test thread, so the next blocking
//! haematite election runs cleanly outside any runtime. The runtime itself stays
//! alive (so the engine's captured handles and monitor tasks stay valid) until
//! the test ends. This is the cluster-harness `block_on` pattern from
//! `distributed_failover.rs`, extended to also drive the Aion engine.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p aion-rs --test aion_cluster_failover_showcase -- --nocapture
//! ```

#![allow(clippy::panic, clippy::doc_markdown, clippy::doc_lazy_continuation)]
#![allow(clippy::too_many_lines)]

// The `hello_world` archive is rebuilt from the committed Gleam source on every
// run (see `common/example_build.rs`); this gate never skips on a missing CLI.
#[path = "common/example_build.rs"]
mod example_build;

use std::collections::HashMap;
use std::error::Error;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aion::EngineBuilder;
use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus};
use aion_store::{EventStore, WriteToken};
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};
use serde_json::json;

type TestResult = Result<(), Box<dyn Error>>;

const NODE_A: &str = "node-a@127.0.0.1";
const NODE_B: &str = "node-b@127.0.0.1";
const NODE_C: &str = "node-c@127.0.0.1";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);
const SHARD: usize = 0;

// ---------------------------------------------------------------------------
// The host-side activity, identical to the single-node showcase: it builds the
// greeting the `hello_world` workflow returns.
// ---------------------------------------------------------------------------

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

/// Render a durable history as a compact human-readable timeline.
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

// ---------------------------------------------------------------------------
// Cluster harness — the 3-node real-beamr-loopback haematite cluster from
// `distributed_failover.rs`. Each node wraps its own distribution-attached
// `Database` in a `HaematiteStore` and runs a background responder draining
// inbound writes. Reused here verbatim in spirit; the only addition is that a
// node can be turned into the OWNER of an Aion engine.
// ---------------------------------------------------------------------------

fn loopback() -> Result<SocketAddr, Box<dyn Error>> {
    Ok("127.0.0.1:0".parse()?)
}

fn config_for(path: &Path) -> DatabaseConfig {
    DatabaseConfig {
        data_dir: path.to_path_buf(),
        shard_count: 1,
        sweep_interval: None,
        distributed: None,
    }
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn membership(total_nodes: usize, send_targets: &[&str]) -> WriteMembership {
    WriteMembership {
        total_nodes,
        send_targets: send_targets
            .iter()
            .map(|name| SyncNodeId::from(*name))
            .collect(),
    }
}

/// One cluster node. Holds a distributed `HaematiteStore` (shared with the Aion
/// engine when this node is the owner), the shared `haematite::EventStore`, and
/// a background responder thread answering peers' replication traffic.
struct Node {
    store: Arc<HaematiteStore>,
    event_store: Arc<haematite::EventStore>,
    addr: SocketAddr,
    name: &'static str,
    responder: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Node {
    fn spawn(
        name: &'static str,
        dir: &Path,
        total_nodes: usize,
        send_targets: &[&str],
    ) -> Result<Self, Box<dyn Error>> {
        let endpoint = DistributionEndpoint::bind(name, loopback()?, 1, None)?;
        let addr = endpoint.local_addr();
        let database =
            Database::create(config_for(dir.join("db").as_path()))?.with_distribution(endpoint);

        let store = Arc::new(HaematiteStore::with_distribution(
            database,
            membership(total_nodes, send_targets),
            OP_TIMEOUT,
            name.to_owned(),
        ));
        let event_store = Arc::clone(store.event_store());

        let running = Arc::new(AtomicBool::new(true));
        let responder_store = Arc::clone(&event_store);
        let responder_running = Arc::clone(&running);
        let responder = std::thread::spawn(move || {
            while responder_running.load(Ordering::Relaxed) {
                drop(respond_to_inbound_writes(
                    responder_store.database(),
                    Duration::from_millis(50),
                ));
            }
        });

        Ok(Self {
            store,
            event_store,
            addr,
            name,
            responder: Some(responder),
            running,
        })
    }

    fn database(&self) -> &Database {
        self.event_store.database()
    }

    /// KILL this node: stop draining inbound writes and join the responder so it
    /// stops answering peers. After this, excluding the node from every future
    /// membership (the caller's job) makes it dead for all protocol purposes.
    fn kill(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.responder.take() {
            drop(handle.join());
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.responder.take() {
            drop(handle.join());
        }
    }
}

fn link(from: &Node, to: &Node) -> TestResult {
    let endpoint = from
        .database()
        .distribution()
        .ok_or("dialing node has no endpoint")?;
    endpoint.add_peer(to.name, to.addr);
    endpoint.connect(to.name)?;
    if !wait_until(HANDSHAKE_TIMEOUT, || endpoint.is_connected(to.name)) {
        return Err(format!("{} never registered a link to {}", from.name, to.name).into());
    }
    Ok(())
}

fn link_both(a: &Node, b: &Node) -> TestResult {
    link(a, b)?;
    link(b, a)?;
    Ok(())
}

/// Build an Aion engine over `store`, loading `package` + the `GreetDispatcher`.
/// Driven on the supplied long-lived runtime so the engine's captured
/// `Handle::current()` (for activity/monitor tasks) points at a runtime that
/// outlives this `block_on`.
async fn build_engine_over(
    store: Arc<dyn EventStore>,
    package: aion_package::Package,
) -> Result<aion::Engine, Box<dyn Error>> {
    let engine = EngineBuilder::new()
        .store_arc(store)
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .load_workflows(package)
        .build()
        .await?;
    Ok(engine)
}

// ===========================================================================
// THE HEADLINE DEMO — completed workflow survives the death of its owner node.
// ===========================================================================

#[test]
fn aion_workflow_survives_owner_node_death_on_haematite_cluster() -> TestResult {
    // ONE long-lived runtime drives EVERY async engine call. Its worker threads
    // outlive each `block_on`, so the engine's captured handles stay valid; and
    // because no `block_on` is in flight between calls, the test thread has no
    // entered runtime when the blocking haematite elections run. See the module
    // docs for why this is the only way to combine the two subsystems.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    println!("\n==================================================================");
    println!(" Aion on a 3-NODE haematite cluster — surviving the OWNER's death");
    println!("==================================================================");
    println!(" nodes        : A (owner), B, C   |   quorum 2   |   real beamr loopback");
    println!(" workflow     : hello_world (real Gleam -> BEAM, run live on node A)");
    println!(" durable state: replicated through HaematiteStore::with_distribution");

    // The workflow CODE is built ONCE and deployed to EVERY node's engine —
    // realistic: code is deployed cluster-wide, only STATE replicates. This also
    // sidesteps the packages/routes-not-replicated gap B2 flagged (those stay
    // local; the durable history is what crosses nodes).
    let package = example_build::built_package("examples/hello-world", "hello_world")?;
    println!("\n hello_world package built once; will be loaded on every node's engine.");

    // Bring up the 3-node cluster. A initially replicates to {B,C}.
    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B, NODE_C])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A, NODE_C])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;
    println!(" cluster linked: A<->B, A<->C, B<->C (real loopback handshakes).");

    // ----------------------------------------------------------------------
    // ACT 1 — node A takes shard 0 and builds a live Aion engine over its store.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 1: node A acquires shard 0 and builds an Aion engine ---");
    node_a.database().acquire_shard_and_serve(
        SHARD,
        &membership(3, &[NODE_B, NODE_C]),
        OP_TIMEOUT,
    )?;
    println!("  node A is the LIVE owner of shard 0 (replicating to {{B,C}}).");

    let a_store: Arc<dyn EventStore> = Arc::clone(&node_a.store) as Arc<dyn EventStore>;
    let engine_a = runtime.block_on(build_engine_over(Arc::clone(&a_store), package.clone()))?;
    println!("  engine BUILT on node A — its durable EventStore is A's HaematiteStore.");

    // ----------------------------------------------------------------------
    // ACT 2 — A runs hello_world to completion; the history replicates to quorum.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 2: node A runs hello_world to completion (state replicates) ---");
    let input = Payload::from_json(&json!({ "name": "Ada" }))?;
    let handle = runtime.block_on(engine_a.start_workflow(
        "hello_world",
        input,
        HashMap::new(),
        String::from("default"),
    ))?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();
    println!("  started workflow id = {workflow_id}");
    println!("                run id = {run_id}");

    let result = runtime.block_on(engine_a.result(&workflow_id, &run_id))?;
    let payload = result.map_err(|error| format!("workflow failed on A: {error:?}"))?;
    let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    println!("  workflow COMPLETED on A. result greeting = {greeting}");
    assert_eq!(greeting, json!("Hello, Ada! Welcome to Aion."));

    let history_on_a = runtime.block_on(a_store.read_history(&workflow_id))?;
    println!("  durable history on A ({} events):", history_on_a.len());
    println!("{}", render_history(&history_on_a));
    assert_eq!(
        history_on_a.len(),
        5,
        "expected the full 5-event lifecycle on A"
    );
    assert_eq!(
        aion_core::status_from_events(&history_on_a),
        WorkflowStatus::Completed
    );

    // PROVE the state really replicated: read the FULL history straight from the
    // survivor B's store. B is not the owner; this is the raw replicated copy.
    let b_store_read: Arc<dyn EventStore> = Arc::clone(&node_b.store) as Arc<dyn EventStore>;
    let history_on_b = runtime.block_on(b_store_read.read_history(&workflow_id))?;
    println!(
        "  SAME history read directly from survivor B's store ({} events):",
        history_on_b.len()
    );
    println!("{}", render_history(&history_on_b));
    assert_eq!(
        history_on_b, history_on_a,
        "survivor B must already hold A's FULL replicated history before any failover"
    );
    println!("  => the workflow's durable state replicated A -> B (verified on B directly).");

    // ----------------------------------------------------------------------
    // ACT 3 — KILL node A. Stop its responder and exclude it from all future
    // membership. A is DEAD for every protocol purpose.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 3: KILL node A (the owner) ---");
    // Shut the engine first (it would otherwise keep A's store handle live), then
    // kill the node's responder. A will be excluded from every membership below.
    engine_a.shutdown()?;
    let mut node_a = node_a;
    node_a.kill();
    println!("  node A's engine shut down and its responder stopped/joined.");
    println!("  node A is excluded from ALL future membership: it is DEAD.");
    println!("  the only live copies of the workflow are now on {{B,C}}.");

    // ----------------------------------------------------------------------
    // ACT 4 — survivor B is promoted: it fences dead A with a higher epoch and
    // become_live union-merges the replicated history, THEN a NEW Aion engine is
    // built over B's store (startup recovery runs against the merged history).
    // ----------------------------------------------------------------------
    println!("\n--- ACT 4: promote survivor B + build a NEW engine over its store ---");
    // Election over {B,C} ONLY — A is gone. become_live union-merges any
    // replicated tree so B serves the full, correct history as the new owner.
    node_b
        .database()
        .acquire_shard_and_serve(SHARD, &membership(2, &[NODE_C]), OP_TIMEOUT)?;
    println!("  node B acquired shard 0 over {{B,C}} (fenced dead A, become_live merged).");

    let b_store: Arc<dyn EventStore> = Arc::clone(&node_b.store) as Arc<dyn EventStore>;
    let engine_b = runtime.block_on(build_engine_over(Arc::clone(&b_store), package.clone()))?;
    println!("  NEW engine BUILT on node B — Aion startup recovery ran over B's store.");

    // ----------------------------------------------------------------------
    // ACT 5 — PROVE survival: B's engine serves the SAME completed result A
    // produced. It came from the replicated+merged cluster, not from dead A and
    // not from local-only disk on B (B never owned this shard until ACT 4).
    // ----------------------------------------------------------------------
    println!("\n--- ACT 5: prove the workflow survived A's death ---");
    let recovered = runtime.block_on(engine_b.result(&workflow_id, &run_id))?;
    let recovered_payload =
        recovered.map_err(|error| format!("recovered workflow failed on B: {error:?}"))?;
    let recovered_greeting: serde_json::Value = serde_json::from_slice(recovered_payload.bytes())?;
    println!("  result from survivor B's engine = {recovered_greeting}");
    assert_eq!(
        recovered_greeting,
        json!("Hello, Ada! Welcome to Aion."),
        "the survivor must serve the SAME result A produced before it died"
    );

    let history_after = runtime.block_on(b_store.read_history(&workflow_id))?;
    println!(
        "  durable history on B after promotion ({} events):",
        history_after.len()
    );
    println!("{}", render_history(&history_after));
    assert_eq!(
        history_after, history_on_a,
        "B must serve the byte-for-byte history A produced — via replicate+merge"
    );
    assert_eq!(
        aion_core::status_from_events(&history_after),
        WorkflowStatus::Completed
    );
    // Recovery must not have re-recorded the lifecycle.
    assert_eq!(
        history_after
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1,
        "promotion + recovery must not duplicate the workflow start"
    );

    // FALSIFIABILITY: B's engine is not fabricating "Completed" for everything —
    // a DIFFERENT, never-started workflow id resolves to not-found. So the
    // Completed result above could ONLY come from the durable history that
    // replicated from A and was merged into B on promotion, not from a vacuous
    // always-succeeds path.
    let phantom_id = WorkflowId::new_v4();
    let phantom_run = RunId::new_v4();
    assert!(
        runtime
            .block_on(b_store.read_history(&phantom_id))?
            .is_empty(),
        "a never-started workflow must have NO history on B (non-vacuity)"
    );
    let phantom = runtime.block_on(engine_b.result(&phantom_id, &phantom_run));
    assert!(
        phantom.is_err(),
        "B's engine must NOT resolve a result for a workflow it never saw — \
         proves ACT 5's success depends on the real replicated+merged history"
    );
    println!("  (falsifiability: an unknown workflow id is not-found on B — the");
    println!("   recovered result above is genuinely from the replicated history.)");

    println!("\n==================================================================");
    println!(" RESULT: the workflow state SURVIVED the death of the node that");
    println!("         owned it. Node A ran the workflow and died; survivor B —");
    println!("         which never owned the shard until A was gone — now serves");
    println!("         the SAME completed result, reconstituted from the");
    println!("         replicated+merged haematite cluster. State outlived its");
    println!("         owner node.");
    println!("==================================================================\n");

    engine_b.shutdown()?;
    // Keep the runtime alive until the very end so engine-owned tasks remain
    // valid through shutdown.
    drop(runtime);
    Ok(())
}

// ===========================================================================
// STRETCH — an IN-FLIGHT workflow is recovered and DRIVEN TO COMPLETION on the
// survivor after the owner dies mid-flight.
// ===========================================================================
//
// The primary demo proves a COMPLETED workflow survives. This stretch proves
// the harder thing: a workflow that was still RUNNING when its owner died is
// picked up by the survivor's engine and finished. Node A records only
// `WorkflowStarted` for the run — modelling a crash the instant after the start
// is durably recorded, exactly the single-node `recovery_e2e` "simulate the
// crash" shape — and that start replicates to the quorum. A is then KILLED; B is
// promoted (fences A + become_live merges the replicated start); a NEW engine is
// built over B's store, and Aion startup recovery re-spawns the workflow process
// from the recovered start, replays it, and runs it LIVE to completion against
// B's dispatcher — recording the rest of the lifecycle (`ActivityScheduled` …
// `WorkflowCompleted`) into the cluster, without duplicating the start.
//
// This is kept as a SEPARATE test so the primary stays deterministic. The start
// event is written through the adapter (deterministic, no mid-activity timing
// race), so this test is itself deterministic.
#[test]
fn inflight_workflow_completes_on_survivor_after_owner_death() -> TestResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    println!("\n==================================================================");
    println!(" STRETCH — an IN-FLIGHT workflow finishes on the survivor node");
    println!("==================================================================");

    let package = example_build::built_package("examples/hello-world", "hello_world")?;

    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B, NODE_C])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A, NODE_C])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;

    // ACT 1 — A owns shard 0 and records ONLY the start (the "crash mid-flight").
    println!("\n--- ACT 1: node A owns shard 0; a workflow is started but NOT finished ---");
    node_a.database().acquire_shard_and_serve(
        SHARD,
        &membership(3, &[NODE_B, NODE_C]),
        OP_TIMEOUT,
    )?;

    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let input = Payload::from_json(&json!({ "name": "Grace" }))?;
    let start = Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq: 1,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: String::from("hello_world"),
        input,
        run_id: run_id.clone(),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new(package.content_hash().to_string()),
    };
    let a_store: Arc<dyn EventStore> = Arc::clone(&node_a.store) as Arc<dyn EventStore>;
    // The start replicates to the quorum {B,C} as it is appended on A.
    runtime.block_on(a_store.append(WriteToken::recorder(), &workflow_id, &[start], 0))?;
    println!("  node A recorded ONLY WorkflowStarted (run is mid-flight, not done).");
    println!("  workflow id = {workflow_id}");

    // The mid-flight start replicated to survivor B.
    let b_store: Arc<dyn EventStore> = Arc::clone(&node_b.store) as Arc<dyn EventStore>;
    let pre = runtime.block_on(b_store.read_history(&workflow_id))?;
    println!(
        "  survivor B already holds the in-flight history ({} event):",
        pre.len()
    );
    println!("{}", render_history(&pre));
    assert_eq!(
        pre.len(),
        1,
        "B must hold exactly the replicated WorkflowStarted"
    );
    assert!(matches!(pre.first(), Some(Event::WorkflowStarted { .. })));
    assert_eq!(
        aion_core::status_from_events(&pre),
        WorkflowStatus::Running,
        "the replicated run must be RUNNING (not terminal) before failover"
    );

    // ACT 2 — KILL A.
    println!("\n--- ACT 2: KILL node A mid-flight ---");
    let mut node_a = node_a;
    node_a.kill();
    println!("  node A is DEAD and excluded from all future membership.");

    // ACT 3 — promote B and build a NEW engine; recovery drives the run to done.
    println!("\n--- ACT 3: promote B; its engine recovers the run and COMPLETES it ---");
    node_b
        .database()
        .acquire_shard_and_serve(SHARD, &membership(2, &[NODE_C]), OP_TIMEOUT)?;
    let engine_b = runtime.block_on(build_engine_over(Arc::clone(&b_store), package.clone()))?;
    println!("  B's engine BUILT — startup recovery re-spawned the in-flight run.");

    // Recovery must register the run as a live, running, supervised process.
    let recovered = engine_b.registry().get(&workflow_id, &run_id)?;
    assert!(
        recovered.is_some_and(|handle| handle.workflow_type() == "hello_world"),
        "the recovered in-flight run must be registered as a resident process on B"
    );

    // ACT 4 — the recovered process runs LIVE to completion on B.
    println!("\n--- ACT 4: prove the recovered run finishes on the survivor ---");
    let result = runtime.block_on(engine_b.result(&workflow_id, &run_id))?;
    let payload = result.map_err(|error| format!("recovered run failed on B: {error:?}"))?;
    let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    println!("  result driven to completion on B = {greeting}");
    assert_eq!(greeting, json!("Hello, Grace! Welcome to Aion."));

    let final_history = runtime.block_on(b_store.read_history(&workflow_id))?;
    println!(
        "  full lifecycle now durable on B ({} events):",
        final_history.len()
    );
    println!("{}", render_history(&final_history));
    assert_eq!(
        aion_core::status_from_events(&final_history),
        WorkflowStatus::Completed
    );
    assert!(matches!(
        final_history.last(),
        Some(Event::WorkflowCompleted { .. })
    ));
    // Recovery must not duplicate the start that replicated from A.
    assert_eq!(
        final_history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1,
        "recovery on B must not re-record the WorkflowStarted that came from A"
    );

    println!("\n==================================================================");
    println!(" RESULT: an IN-FLIGHT workflow whose owner died was recovered on a");
    println!("         survivor and DRIVEN TO COMPLETION there. Only the start had");
    println!("         been recorded (and replicated) before A died; B's engine");
    println!("         re-spawned the run from the merged history and finished it.");
    println!("==================================================================\n");

    engine_b.shutdown()?;
    drop(runtime);
    Ok(())
}
