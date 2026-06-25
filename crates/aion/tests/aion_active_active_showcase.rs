//! THE SHIP GATE — a 3-node, 3-shard ACTIVE-ACTIVE haematite cluster where every
//! node owns ONE shard and runs ITS OWN real Aion workflow concurrently. One node
//! is KILLED; its shard re-homes on a survivor and ITS workflow resumes/completes;
//! a DIFFERENT survivor is provably UNINTERRUPTED — its workflow finishes on the
//! ORIGINAL engine that was never rebuilt or touched after the kill.
//!
//! This is the multi-shard generalisation of `aion_cluster_failover_showcase.rs`
//! (read that first — this reuses its harness verbatim in spirit). Where the
//! single-shard showcase proved ONE workflow survives ONE owner's death, this
//! proves the distributed promise: `shard_count = 3`, three engines live at once,
//! each the sole owner of its shard, all making progress — and when node 1 dies,
//! node 0 ABSORBS shard 1 (re-homing node 1's workflow and driving it to done)
//! while node 2 — never re-elected, never rebuilt — continues UNINTERRUPTED. Three
//! shards, three workflows, three nodes; kill one, the cluster keeps serving.
//!
//! ## The act-by-act story (printed for a human; run with `-- --nocapture`)
//!
//!   * ACT 1 — bring up a 3-node mesh at `shard_count = 3`; each node `i`
//!     `acquire_shard_and_serve`s shard `i` over the OTHER two (real distributed
//!     election under shard_count>1, the haematite spike proved this composes),
//!     scopes its store to its own shard via `set_owned_shards([i])`, and builds
//!     an Aion engine. Only the node owning the schedule-coordinator's shard
//!     bootstraps the coordinator (the AA-4-4 gate — non-owners must NOT fence the
//!     coordinator stream).
//!   * ACT 2 — for each node `i`, force workflow `w[i]` ONTO shard `i` (deterministic
//!     store-append `WorkflowStarted`, because `Engine::start_workflow` mints its
//!     own id and can't be steered), and drive each engine to complete its own
//!     workflow concurrently. Three distinct greetings come back — the cluster is
//!     ACTIVE-ACTIVE.
//!   * ACT 3 — KILL node 1 (shut its engine, stop its responder, exclude it from
//!     all future membership). Its shard's only live copies are on {0,2}.
//!   * ACT 4 — node 0 ABSORBS shard 1: it `acquire_shard_and_serve`s shard 1 over
//!     {0,2} (fences dead node 1 + become_live union-merges the replicated tree),
//!     extends its owned set to `[0, 1]`, and a FRESH engine is built over node 0's
//!     store — startup recovery re-residents both shards' workflows.
//!   * ACT 5 — PROVE the gate: w[1] is resident on node 0's new engine and completes
//!     to the SAME greeting node 1 would have produced; w[1] has exactly one
//!     WorkflowStarted (no duplicate); a phantom id is not-found (falsifiability);
//!     the coordinator stream has exactly one WorkflowStarted cluster-wide (the
//!     AA-4-4 single-owner bootstrap held across 3 nodes); and the UNINTERRUPTED
//!     witness — node 2's w[2] — completes on node 2's ORIGINAL, never-rebuilt
//!     engine AFTER the kill (the across-kill active-active proof).
//!
//! ## Honest gaps (documented, not hidden)
//!
//!   * Packages are NODE-LOCAL: the `hello_world` code is built once and loaded into
//!     EVERY node's engine. Only durable STATE replicates across the cluster — the
//!     established workaround the single-shard showcase uses too.
//!   * The schedule coordinator is bootstrapped by a SINGLE owner per cluster (the
//!     node owning its shard). Non-owners pass `bootstrap_schedule_coordinator(false)`
//!     so they do not fence/duplicate the coordinator stream.
//!   * Failover here is option (a): the ABSORBER rebuilds its engine to re-resident
//!     the re-homed shard. The WITNESS proves uninterrupted service — its engine is
//!     NEVER rebuilt and completes its workflow across the kill.
//!
//! ## Why a single long-lived runtime + `block_on` (not `#[tokio::test]`)
//!
//! Identical to the single-shard showcase: haematite's blocking distribution
//! coordinator refuses to run from a thread with an ENTERED tokio runtime, while
//! Aion's engine is async and captures `Handle::current()`. ONE long-lived runtime
//! drives every async engine call through `block_on`; the blocking elections run
//! on the bare test thread BETWEEN `block_on`s. See the sibling file's module docs
//! for the full rationale.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p aion-rs --test aion_active_active_showcase -- --nocapture
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]
#![allow(clippy::panic, clippy::doc_markdown, clippy::doc_lazy_continuation)]

// The `hello_world` archive is rebuilt from the committed Gleam source on every
// run (see `common/example_build.rs`); this gate never skips on a missing CLI.
#[path = "common/example_build.rs"]
mod example_build;

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
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error>>;

const NODE_NAMES: [&str; 3] = ["node-0@127.0.0.1", "node-1@127.0.0.1", "node-2@127.0.0.1"];

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);
const SHARD_COUNT: usize = 3;

// ---------------------------------------------------------------------------
// The host-side activity, identical to the single-node/single-shard showcases:
// it builds the greeting the `hello_world` workflow returns.
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
// Local reproductions of pub(crate) production constants/encodings so the
// integration test can route workflows onto chosen shards WITHOUT touching any
// production key encoding.
// ---------------------------------------------------------------------------

/// Reproduce the adapter's PRIVATE `keyspace::event_stream_key` encoding (`E` tag
/// byte followed by the raw 16-byte UUID) — the co-location route key. Lets us ask
/// haematite which shard a workflow's event stream routes to.
fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

/// Reproduce the engine's PRIVATE `schedule_coordinator_workflow_id()` constant
/// so we can compute which shard owns the coordinator stream and check its history.
fn schedule_coordinator_workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x0000_0000_a10a_0000_0000_0000_0000_0004))
}

// ---------------------------------------------------------------------------
// Cluster harness — the 3-node real-beamr-loopback haematite cluster, reused
// from the single-shard showcase. The ONLY change is `config_for_shards`, which
// parameterises `shard_count` (the showcase hardcodes 1).
// ---------------------------------------------------------------------------

fn loopback() -> Result<SocketAddr, Box<dyn Error>> {
    Ok("127.0.0.1:0".parse()?)
}

/// Like the showcase's `config_for`, but with a caller-chosen `shard_count` so the
/// cluster can run distributed multi-shard (`distributed: None` here — distribution
/// is attached separately via `Database::with_distribution`, exactly as the
/// haematite multi-shard spike does).
fn config_for_shards(path: &Path, shard_count: usize) -> DatabaseConfig {
    DatabaseConfig {
        data_dir: path.to_path_buf(),
        shard_count,
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
        send_targets: send_targets.iter().map(|name| SyncNodeId::from(*name)).collect(),
    }
}

/// One cluster node. Holds a distributed `HaematiteStore` (shared with the Aion
/// engine), the shared `haematite::EventStore`, and a background responder thread
/// answering peers' replication traffic.
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
        shard_count: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let endpoint = DistributionEndpoint::bind(name, loopback()?, 1, None)?;
        let addr = endpoint.local_addr();
        let database = Database::create(config_for_shards(dir.join("db").as_path(), shard_count))?
            .with_distribution(endpoint);

        let store = Arc::new(HaematiteStore::with_distribution(
            database,
            membership(total_nodes, send_targets),
            OP_TIMEOUT,
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

    /// KILL this node: stop draining inbound writes and join the responder. After
    /// this, excluding the node from every future membership makes it dead for all
    /// protocol purposes.
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
/// `bootstrap_coordinator` is `true` ONLY on the node owning the coordinator's
/// shard, so non-owners don't fence the coordinator stream (the AA-4-4 gate).
async fn build_engine_over(
    store: Arc<dyn EventStore>,
    package: aion_package::Package,
    bootstrap_coordinator: bool,
) -> Result<aion::Engine, Box<dyn Error>> {
    let engine = EngineBuilder::new()
        .store_arc(store)
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .bootstrap_schedule_coordinator(bootstrap_coordinator)
        .load_workflows(package)
        .build()
        .await?;
    Ok(engine)
}

/// Mint a `WorkflowId` whose event stream routes to `shard` (rejection sampling
/// over `Database::shard_for(event_stream_key(..))`).
fn workflow_id_for_shard(database: &Database, shard: usize) -> WorkflowId {
    loop {
        let candidate = WorkflowId::new_v4();
        if database.shard_for(&event_stream_key(&candidate)) == shard {
            return candidate;
        }
    }
}

/// Record `WorkflowStarted` for `workflow_id`/`run_id` directly through `store`
/// (deterministic — `Engine::start_workflow` mints its own id and can't be
/// steered onto a chosen shard). The append routes to the workflow's shard and
/// `replicate_append`s to the quorum.
async fn record_start(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    name: &str,
    package: &aion_package::Package,
) -> TestResult {
    let input = Payload::from_json(&json!({ "name": name }))?;
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
    store
        .append(WriteToken::recorder(), workflow_id, &[start], 0)
        .await?;
    Ok(())
}

// ===========================================================================
// THE SHIP GATE — 3 nodes, 3 shards, 3 workflows; kill one, re-home its shard,
// and prove a different survivor was uninterrupted.
// ===========================================================================

#[test]
fn multi_shard_active_active_failover_demo() -> TestResult {
    // ONE long-lived runtime drives EVERY async engine call (its worker threads
    // outlive each `block_on`); the blocking haematite elections run on the bare
    // test thread BETWEEN `block_on`s. See the module docs for the full rationale.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir()).collect::<Result<_, _>>()?;

    println!("\n==================================================================");
    println!(" Aion ACTIVE-ACTIVE on a 3-NODE / 3-SHARD haematite cluster");
    println!("==================================================================");
    println!(" nodes  : node-0, node-1, node-2   |   shard_count = 3");
    println!(" layout : node i owns shard i, runs ITS OWN hello_world workflow");
    println!(" gate   : kill node 1 -> shard 1 re-homes on node 0 + resumes;");
    println!("          node 2 (witness) completes UNINTERRUPTED on its own engine");

    // The workflow CODE is built ONCE and deployed to EVERY node's engine —
    // packages are node-local; only durable STATE replicates (the established
    // workaround, same as the single-shard showcase).
    let package = example_build::built_package("examples/hello-world", "hello_world")?;
    println!("\n hello_world package built once; loaded on every node's engine.");

    // Bring up the 3-node mesh at shard_count = 3; link all pairs.
    let send_targets: Vec<Vec<&str>> = (0..3)
        .map(|i| (0..3).filter(|&j| j != i).map(|j| NODE_NAMES[j]).collect())
        .collect();
    let mut nodes: Vec<Node> = (0..3)
        .map(|i| Node::spawn(NODE_NAMES[i], dirs[i].path(), 3, &send_targets[i], SHARD_COUNT))
        .collect::<Result<_, _>>()?;
    link_both(&nodes[0], &nodes[1])?;
    link_both(&nodes[0], &nodes[2])?;
    link_both(&nodes[1], &nodes[2])?;
    println!(" cluster linked: 0<->1, 0<->2, 1<->2 (real loopback handshakes).");

    // Which shard owns the schedule coordinator? Only its owner bootstraps it.
    let coord_id = schedule_coordinator_workflow_id();
    let coord_shard = nodes[0].database().shard_for(&event_stream_key(&coord_id));
    println!(" schedule-coordinator stream routes to shard {coord_shard}");
    println!("   => only node {coord_shard} bootstraps the coordinator (AA-4-4 gate).");

    // ----------------------------------------------------------------------
    // ACT 1 — every node `i` acquires shard `i`, scopes to it, and records its
    // own workflow's WorkflowStarted onto shard `i`. `Engine::start_workflow`
    // mints its own id and can't be steered onto a chosen shard, so the
    // deterministic store-append `WorkflowStarted` path is used (the single-shard
    // showcase's stretch pattern); each start replicates to the quorum.
    //
    // Nodes 0 and 2 build engines (startup recovery re-spawns w[0]/w[2] as live
    // resident processes). Node 1 deliberately does NOT build an engine — w[1] is
    // left IN-FLIGHT (only WorkflowStarted durable), modelling a crash the instant
    // after the start was recorded. That is what makes the re-home in ACT 4 the
    // STRONGER proof: the absorber must RECOVER and DRIVE an unfinished workflow,
    // not merely re-serve a completed one.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 1: each node i owns shard i and seeds its workflow ---");
    let workflow_ids: Vec<WorkflowId> =
        (0..3).map(|i| workflow_id_for_shard(nodes[i].database(), i)).collect();
    let run_ids: Vec<RunId> = (0..3).map(|_| RunId::new_v4()).collect();
    let names = ["Shard0", "Shard1", "Shard2"];

    // Engines keyed by node index; node 1 has none (it stays in-flight).
    let mut engines: Vec<Option<aion::Engine>> = (0..3).map(|_| None).collect();
    for i in 0..3 {
        // Distributed election: node i takes shard i over the OTHER two nodes.
        nodes[i].database().acquire_shard_and_serve(
            i,
            &membership(3, &send_targets[i]),
            OP_TIMEOUT,
        )?;
        // Scope this node's startup recovery + enumeration to its own shard.
        nodes[i].store.set_owned_shards([i]);
        // Record w[i]'s start onto shard i (replicates to the quorum).
        let store: Arc<dyn EventStore> = Arc::clone(&nodes[i].store) as Arc<dyn EventStore>;
        runtime.block_on(record_start(
            &store,
            &workflow_ids[i],
            &run_ids[i],
            names[i],
            &package,
        ))?;
        if i == 1 {
            println!("  node 1: owns shard 1, seeded w[1] (name = Shard1), LEFT IN-FLIGHT (no engine)");
            continue;
        }
        // Build the engine AFTER the start is durable so recovery re-spawns w[i].
        let engine = runtime.block_on(build_engine_over(
            Arc::clone(&store),
            package.clone(),
            i == coord_shard,
        ))?;
        println!(
            "  node {i}: owns shard {i}, seeded w[{i}] (name = {}), engine recovered it \
             (bootstrap_coordinator = {})",
            names[i],
            i == coord_shard
        );
        engines[i] = Some(engine);
    }

    // ----------------------------------------------------------------------
    // ACT 2 — node 0 completes its OWN workflow (active-active). Node 2's w[2] is
    // deliberately held IN-FLIGHT across the kill (the across-kill witness);
    // node 1's w[1] is in-flight and will be RE-HOMED to node 0 in ACT 4.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 2: node 0 completes its OWN workflow (active-active) ---");
    {
        let engine_0 = engines[0].as_ref().expect("node 0 engine");
        let result = runtime.block_on(engine_0.result(&workflow_ids[0], &run_ids[0]))?;
        let payload = result.map_err(|error| format!("w[0] failed on node 0: {error:?}"))?;
        let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
        println!("  node 0: w[0] COMPLETED -> {greeting}");
        assert_eq!(greeting, json!("Hello, Shard0! Welcome to Aion."));
    }
    println!("  (w[1] in-flight -> re-homed in ACT 4; w[2] in-flight -> witness across the kill.)");

    // ----------------------------------------------------------------------
    // ACT 3 — KILL node 1. It had no engine; stop its responder + exclude it.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 3: KILL node 1 (owner of shard 1) ---");
    nodes[1].kill();
    println!("  node 1's responder stopped/joined; node 1 is DEAD (w[1] was mid-flight).");
    println!("  the only live copies of shard 1 are now on {{0, 2}}.");

    // ----------------------------------------------------------------------
    // ACT 4 — node 0 ABSORBS shard 1, extends its owned set, rebuilds its engine.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 4: node 0 absorbs shard 1 + rebuilds its engine ---");
    // Election for shard 1 over the SURVIVORS {0, 2} only — node 1 is gone.
    nodes[0]
        .database()
        .acquire_shard_and_serve(1, &membership(2, &[NODE_NAMES[2]]), OP_TIMEOUT)?;
    println!("  node 0 acquired shard 1 over {{0,2}} (fenced dead node 1, merged).");
    // Node 0 now owns BOTH shard 0 (its original) and shard 1 (re-homed).
    nodes[0].store.set_owned_shards([0, 1]);
    // Shut node 0's old engine and build a FRESH one — startup recovery scoped to
    // {0,1} re-residents w[0] AND re-spawns the in-flight w[1] from the merged history.
    engines[0].take().expect("node 0 engine").shutdown()?;
    let absorber_store: Arc<dyn EventStore> = Arc::clone(&nodes[0].store) as Arc<dyn EventStore>;
    let absorber_engine = runtime.block_on(build_engine_over(
        Arc::clone(&absorber_store),
        package.clone(),
        coord_shard == 0 || coord_shard == 1,
    ))?;
    println!("  NEW engine built on node 0 over shards {{0,1}} — recovery re-spawned w[1].");

    // ----------------------------------------------------------------------
    // ACT 5 — PROVE the gate (every assertion below must hold).
    // ----------------------------------------------------------------------
    println!("\n--- ACT 5: prove the active-active failover gate ---");

    // (a) RE-HOMED: w[1] is resident on the absorber's NEW engine.
    let resident = absorber_engine.registry().get(&workflow_ids[1], &run_ids[1])?;
    assert!(
        resident.is_some_and(|handle| handle.workflow_type() == "hello_world"),
        "w[1] must be re-resident on node 0's new engine after absorbing shard 1"
    );
    println!("  [re-home] w[1] is resident on node 0's new engine.");

    // (b) RE-HOMED completes to the right greeting.
    let rehomed = runtime.block_on(absorber_engine.result(&workflow_ids[1], &run_ids[1]))?;
    let rehomed_payload =
        rehomed.map_err(|error| format!("re-homed w[1] failed on node 0: {error:?}"))?;
    let rehomed_greeting: serde_json::Value = serde_json::from_slice(rehomed_payload.bytes())?;
    println!("  [re-home] w[1] COMPLETED on node 0 -> {rehomed_greeting}");
    assert_eq!(
        rehomed_greeting,
        json!("Hello, Shard1! Welcome to Aion."),
        "the re-homed workflow must serve node 1's greeting from the survivor"
    );

    // (c) NO DUPLICATE START: w[1]'s history has exactly one WorkflowStarted.
    let w1_history = runtime.block_on(absorber_store.read_history(&workflow_ids[1]))?;
    println!("  [no-dup] w[1] history on node 0 ({} events):", w1_history.len());
    println!("{}", render_history(&w1_history));
    assert_eq!(
        w1_history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1,
        "re-home + recovery must not duplicate w[1]'s WorkflowStarted"
    );
    assert_eq!(
        aion_core::status_from_events(&w1_history),
        WorkflowStatus::Completed
    );

    // (d) FALSIFIABILITY: a phantom workflow id is not-found on the survivor engine.
    let phantom_id = WorkflowId::new_v4();
    let phantom_run = RunId::new_v4();
    assert!(
        runtime.block_on(absorber_store.read_history(&phantom_id))?.is_empty(),
        "a never-started workflow must have NO history on node 0 (non-vacuity)"
    );
    assert!(
        runtime
            .block_on(absorber_engine.result(&phantom_id, &phantom_run))
            .is_err(),
        "node 0's engine must NOT resolve a phantom workflow it never saw"
    );
    println!("  [falsifiability] a phantom workflow id is not-found on node 0.");

    // (e) COORDINATOR SEEDED ONCE cluster-wide: read the coordinator stream from
    // EACH surviving node's store; exactly one WorkflowStarted total proves the
    // AA-4-4 gate prevented duplicate/fenced bootstrap across 3 nodes.
    let coord_on_absorber = runtime.block_on(absorber_store.read_history(&coord_id))?;
    let witness_store: Arc<dyn EventStore> = Arc::clone(&nodes[2].store) as Arc<dyn EventStore>;
    let coord_on_witness = runtime.block_on(witness_store.read_history(&coord_id))?;
    let coord_starts = coord_on_absorber
        .iter()
        .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
        .count();
    assert_eq!(
        coord_starts, 1,
        "the schedule coordinator must be started EXACTLY once cluster-wide"
    );
    // The replicated copy on the witness must agree byte-for-byte (single seed).
    assert_eq!(
        coord_on_witness, coord_on_absorber,
        "the single coordinator seed must have replicated identically to the witness"
    );
    println!("  [coordinator] exactly one WorkflowStarted cluster-wide (single owner seeded it).");

    // (f) UNINTERRUPTED WITNESS — the active-active proof. Node 2's engine was
    // NEVER shut down, re-elected, or rebuilt after the kill. We drive its
    // in-flight w[2] to completion on that SAME original engine, AFTER node 1
    // died and node 0 was rebuilt. The witness served continuously across the
    // failover (the strongest "uninterrupted" proof — across-kill, not pre-kill).
    let witness_engine = engines[2].as_ref().expect("node 2 (witness) engine");
    let witness = runtime.block_on(witness_engine.result(&workflow_ids[2], &run_ids[2]))?;
    let witness_payload =
        witness.map_err(|error| format!("witness w[2] failed on node 2: {error:?}"))?;
    let witness_greeting: serde_json::Value = serde_json::from_slice(witness_payload.bytes())?;
    println!("  [witness] node 2's w[2] COMPLETED on its ORIGINAL engine -> {witness_greeting}");
    assert_eq!(
        witness_greeting,
        json!("Hello, Shard2! Welcome to Aion."),
        "the witness must finish its own workflow uninterrupted, across the kill"
    );

    println!("\n==================================================================");
    println!(" RESULT: 3 nodes, 3 shards, 3 workflows — all active at once. Node 1");
    println!("         died; node 0 ABSORBED its shard and resumed its workflow to");
    println!("         completion with no duplicate start; the schedule coordinator");
    println!("         stayed single-seeded across all 3 nodes; and node 2 — never");
    println!("         re-elected or rebuilt — finished ITS workflow uninterrupted");
    println!("         ACROSS the kill. The cluster kept serving through a node death.");
    println!("==================================================================\n");

    // Tear down the live engines; keep the runtime alive until the very end so
    // engine-owned tasks remain valid through shutdown.
    absorber_engine.shutdown()?;
    engines[2].take().expect("node 2 engine").shutdown()?;
    drop(runtime);
    Ok(())
}
