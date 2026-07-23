//! SS-5: the headline MULTI-NODE FAILOVER demo, driven through the PRODUCTION
//! engine path and a LIVE survivor's `Engine::adopt_shards` entry point.
//!
//! This is the proof the whole storage thesis rests on: a durable workflow whose
//! owner node DIES resumes and completes on a DIFFERENT, already-running node,
//! with its history intact and no lost events — all over a real beamr-loopback
//! haematite cluster with genuine per-shard election, epoch fencing, and
//! `become_live` union-merge.
//!
//! ## How this differs from the AA-4-x showcase (`aion_active_active_showcase.rs`)
//!
//! The showcase proved the same shape but its failover (ACT 4) REBUILDS a fresh
//! engine over the absorber's store — a test-harness rebuild, not a production
//! entry point. SS-5 instead calls **`Engine::adopt_shards`** on node 0's
//! ALREADY-RUNNING, never-shut-down engine. `adopt_shards` is the real failover
//! API a cluster supervisor invokes on a membership-loss trigger: it
//!
//!   1. wins the per-shard election for the dead peer's shard (fencing the dead
//!      owner) and `become_live` union-merges its committed history locally —
//!      through the type-erased `ReadableEventStore::acquire_owned_shards` seam,
//!      whose distributed impl runs the blocking election OFF the tokio runtime,
//!      honouring haematite's no-blocking-election-in-async constraint;
//!   2. UNIONS the adopted shard into the node's owned-enumeration scope
//!      (`extend_owned_shards`) WITHOUT dropping its own shards; and
//!   3. re-residents the orphaned workflows through the SAME production recovery
//!      seam the boot path uses — idempotently, skipping the workflows this node
//!      already serves.
//!
//! Each node's engine is built through the PRODUCTION `EngineBuilder`
//! (`.owned_shards([i])` drives the SS-2 boot election + scoping). The genuine
//! resume is verified, not faked: w[1] is left IN-FLIGHT (only `WorkflowStarted`
//! durable, no engine ever ran it), so node 0 must REPLAY it from the
//! union-merged haematite history to drive it to completion.
//!
//! ## Manual-triggered, not auto-detected (be explicit)
//!
//! Failover detection is the CALLER's job here: the test KILLS node 1 and then
//! EXPLICITLY calls `node0.adopt_shards(&[1])`. That manual-triggered failover —
//! a genuine end-to-end resume on a live survivor — is the headline proof and the
//! required deliverable. Automatic membership-loss detection (a cluster
//! supervisor watching beamr liveness and calling `adopt_shards` itself) is the
//! remaining SS-5b step, deferred.
//!
//! ## Why one long-lived runtime + `block_on` (not `#[tokio::test]`)
//!
//! Identical to the SS-2 builder test and the showcase: binding the replication
//! endpoint and running the blocking election refuse to run from a thread with an
//! ENTERED tokio runtime, while the engine builder is async and captures
//! `Handle::current()`. ONE long-lived runtime drives every async engine call
//! through `block_on`; the blocking elections run off-runtime inside the store
//! seam. See `aion_active_active_showcase.rs` for the full rationale.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p aion-rs --test ss5_failover_demo -- --nocapture
//! ```

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

/// Borrow node `index`'s engine, returning a typed error rather than panicking
/// when it is absent (keeps the test free of `expect`/`unwrap`, which the
/// workspace denies).
fn engine_ref(
    engines: &[Option<aion::Engine>],
    index: usize,
) -> Result<&aion::Engine, Box<dyn Error>> {
    engines
        .get(index)
        .and_then(Option::as_ref)
        .ok_or_else(|| format!("node {index} engine is absent").into())
}

/// Print the closing RESULT banner (kept out of the test body so the
/// gate function holds the clippy line-count bar without a bypass).
fn print_result_banner() {
    println!("\n==================================================================");
    println!(" RESULT: node 1 died; node 0's LIVE engine adopt_shards([1]) elected");
    println!("         + merged shard 1 and REPLAYED node 1's orphaned workflow to");
    println!("         completion from haematite history — no rebuild, no duplicate");
    println!("         start, no lost events, own shard still served; node 2 finished");
    println!("         uninterrupted across the kill. Durable failover is real.");
    println!("==================================================================\n");
}

/// Drive `engine`'s own workflow to completion and assert it returns
/// `expected_greeting` (ACT 2 — the cluster is serving before the kill).
fn complete_own_workflow(
    runtime: &tokio::runtime::Runtime,
    engine: &aion::Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    expected_greeting: &str,
) -> TestResult {
    let result = runtime.block_on(engine.result(workflow_id, run_id))?;
    let payload = result.map_err(|error| format!("own workflow failed: {error:?}"))?;
    let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    println!("  node 0: w[0] COMPLETED -> {greeting}");
    assert_eq!(greeting, json!(expected_greeting));
    Ok(())
}

const NODE_NAMES: [&str; 3] = [
    "ss5-node-0@127.0.0.1",
    "ss5-node-1@127.0.0.1",
    "ss5-node-2@127.0.0.1",
];

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);
const SHARD_COUNT: usize = 3;

// ---------------------------------------------------------------------------
// The host-side activity backing the `hello_world` workflow's `greet` call.
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

/// Reproduce the adapter's PRIVATE `keyspace::event_stream_key` encoding so the
/// test can compute which shard the schedule coordinator's stream routes to
/// (the AA-4-4 single-owner bootstrap gate). Workflow placement itself uses the
/// store's public `shard_for_workflow`.
fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

/// Reproduce the engine's PRIVATE `schedule_coordinator_workflow_id()` constant.
fn schedule_coordinator_workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x0000_0000_a10a_0000_0000_0000_0000_0004))
}

// ---------------------------------------------------------------------------
// Cluster harness — a 3-node real-beamr-loopback haematite cluster, reused in
// spirit from the AA-4-x showcase.
// ---------------------------------------------------------------------------

fn loopback() -> Result<SocketAddr, Box<dyn Error>> {
    Ok("127.0.0.1:0".parse()?)
}

fn config_for_shards(path: &Path, shard_count: usize) -> DatabaseConfig {
    DatabaseConfig {
        data_dir: path.to_path_buf(),
        shard_count,
        executor_threads: None,
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

/// One cluster node: a distributed `HaematiteStore` (shared with the Aion
/// engine), the shared `haematite::EventStore`, and a background responder
/// thread answering peers' replication/election traffic.
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

    /// KILL this node: stop draining inbound writes and join the responder.
    /// After this, excluding the node from every future membership makes it dead
    /// for all protocol purposes.
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

/// Build an Aion engine over `store` through the PRODUCTION `EngineBuilder`,
/// pinning it to `owned_shard` (the SS-2 boot election + scoping hook) and
/// loading `package` + the `GreetDispatcher`. `bootstrap_coordinator` is `true`
/// ONLY on the node owning the coordinator's shard (the AA-4-4 gate).
async fn build_node_engine(
    store: Arc<dyn EventStore>,
    package: aion_package::Package,
    owned_shard: usize,
    bootstrap_coordinator: bool,
) -> Result<aion::Engine, Box<dyn Error>> {
    let engine = EngineBuilder::new()
        .store_arc(store)
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .bootstrap_schedule_coordinator(bootstrap_coordinator)
        .owned_shards([owned_shard])
        .load_workflows(package)
        .build()
        .await?;
    Ok(engine)
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

/// Mint a `WorkflowId` whose event stream routes to `shard` over the store's
/// shard count (rejection sampling over the store's public `shard_for_workflow`).
fn workflow_id_for_shard(store: &HaematiteStore, shard: usize) -> WorkflowId {
    loop {
        let candidate = WorkflowId::new_v4();
        if store.shard_for_workflow(&candidate) == shard {
            return candidate;
        }
    }
}

/// Per-node seeding inputs for ACT 1.
struct SeedInputs<'a> {
    runtime: &'a tokio::runtime::Runtime,
    node: &'a Node,
    send_targets: &'a [&'a str],
    package: &'a aion_package::Package,
    workflow_id: &'a WorkflowId,
    run_id: &'a RunId,
    name: &'a str,
    shard: usize,
    coord_shard: usize,
}

/// ACT 1 for one node: own its shard, seed its workflow's `WorkflowStarted`, and
/// (unless it is the deliberately-in-flight node 1) build a PRODUCTION engine
/// that recovers it. Returns the engine, or `None` for the in-flight node.
fn seed_node(inputs: &SeedInputs<'_>) -> Result<Option<aion::Engine>, Box<dyn Error>> {
    // Own the shard so the replicated start append draws its stamp from a live
    // owner. The builder re-acquires idempotently for the engine-building nodes.
    inputs.node.database().acquire_shard_and_serve(
        inputs.shard,
        &membership(3, inputs.send_targets),
        OP_TIMEOUT,
    )?;
    inputs.node.store.set_owned_shards([inputs.shard]);
    let store: Arc<dyn EventStore> = Arc::clone(&inputs.node.store) as Arc<dyn EventStore>;
    inputs.runtime.block_on(record_start(
        &store,
        inputs.workflow_id,
        inputs.run_id,
        inputs.name,
        inputs.package,
    ))?;
    if inputs.shard == 1 {
        println!(
            "  node 1: owns shard 1, seeded w[1] (name = {}), LEFT IN-FLIGHT (no engine)",
            inputs.name
        );
        return Ok(None);
    }
    let bootstrap = inputs.shard == inputs.coord_shard;
    let engine = inputs.runtime.block_on(build_node_engine(
        Arc::clone(&store),
        inputs.package.clone(),
        inputs.shard,
        bootstrap,
    ))?;
    println!(
        "  node {}: PRODUCTION engine built over shard {}, recovered w[{}] (name = {}, \
         bootstrap_coordinator = {bootstrap})",
        inputs.shard, inputs.shard, inputs.shard, inputs.name
    );
    Ok(Some(engine))
}

/// Everything ACT 5 needs to prove the gate on the survivor.
struct GateInputs<'a> {
    runtime: &'a tokio::runtime::Runtime,
    engine_0: &'a aion::Engine,
    witness_engine: &'a aion::Engine,
    store_0: &'a Arc<dyn EventStore>,
    workflow_ids: &'a [WorkflowId],
    run_ids: &'a [RunId],
}

/// ACT 5: prove the SS-5 failover gate. Every assertion here must hold for the
/// demo to count. Factored out of the test body to hold the function-length bar
/// without a clippy bypass.
fn prove_ss5_gate(inputs: &GateInputs<'_>) -> TestResult {
    let GateInputs {
        runtime,
        engine_0,
        witness_engine,
        store_0,
        workflow_ids,
        run_ids,
    } = *inputs;

    // (a) RE-HOMED: w[1] is resident on node 0's SAME live engine.
    let resident = engine_0.registry().get(&workflow_ids[1], &run_ids[1])?;
    assert!(
        resident.is_some_and(|handle| handle.workflow_type() == "hello_world"),
        "w[1] must be re-resident on node 0's live engine after adopt_shards([1])"
    );
    println!("  [re-home] w[1] is resident on node 0's live engine (adopted, not rebuilt).");

    // (b) GENUINE RESUME: the adopted, in-flight workflow REPLAYS from the merged
    //     haematite history and completes to node 1's greeting. Nothing ever ran
    //     w[1] before this — it had ONLY a WorkflowStarted on a dead node — so a
    //     completed result here can ONLY come from a real replay on node 0.
    let rehomed = runtime.block_on(engine_0.result(&workflow_ids[1], &run_ids[1]))?;
    let rehomed_payload =
        rehomed.map_err(|error| format!("adopted w[1] failed on node 0: {error:?}"))?;
    let rehomed_greeting: serde_json::Value = serde_json::from_slice(rehomed_payload.bytes())?;
    println!("  [resume] w[1] COMPLETED on node 0 -> {rehomed_greeting}");
    assert_eq!(
        rehomed_greeting,
        json!("Hello, Shard1! Welcome to Aion."),
        "the adopted workflow must serve node 1's greeting, replayed from haematite on the survivor"
    );

    // (c) HISTORY INTACT / NO LOST EVENTS / NO DUPLICATE START.
    let w1_history = runtime.block_on(store_0.read_history(&workflow_ids[1]))?;
    println!("  [history] w[1] on node 0 ({} events):", w1_history.len());
    println!("{}", render_history(&w1_history));
    assert_eq!(
        w1_history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1,
        "adopt + recovery must not duplicate w[1]'s WorkflowStarted"
    );
    for (index, event) in w1_history.iter().enumerate() {
        let expected = u64::try_from(index + 1)?;
        assert_eq!(
            event.seq(),
            expected,
            "w[1] history must be contiguous (no lost events) — gap at index {index}"
        );
    }
    assert!(
        matches!(w1_history.first(), Some(Event::WorkflowStarted { .. })),
        "w[1] history must begin with the union-merged WorkflowStarted"
    );
    assert_eq!(
        aion_core::status_from_events(&w1_history),
        WorkflowStatus::Completed,
        "w[1] must be terminally Completed in its intact history"
    );
    println!("  [history] one WorkflowStarted, contiguous seqs, terminal Completed (intact).");

    // (d) NODE 0'S OWN WORKFLOW SURVIVED THE ADOPTION (scope widened, not replaced).
    assert!(
        engine_0
            .registry()
            .get(&workflow_ids[0], &run_ids[0])?
            .is_some(),
        "node 0's OWN workflow w[0] must remain resident after adopting shard 1"
    );
    println!("  [own-shard] node 0 still serves its own w[0] (scope widened, not replaced).");

    // (e) FALSIFIABILITY: a phantom workflow id is not-found on node 0.
    let phantom_id = WorkflowId::new_v4();
    let phantom_run = RunId::new_v4();
    assert!(
        runtime
            .block_on(store_0.read_history(&phantom_id))?
            .is_empty(),
        "a never-started workflow must have NO history on node 0 (non-vacuity)"
    );
    assert!(
        runtime
            .block_on(engine_0.result(&phantom_id, &phantom_run))
            .is_err(),
        "node 0's engine must NOT resolve a phantom workflow it never saw"
    );
    println!("  [falsifiability] a phantom workflow id is not-found on node 0.");

    // (f) UNINTERRUPTED WITNESS — node 2's engine was never touched after the kill.
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
    Ok(())
}

// ===========================================================================
// THE SS-5 GATE — kill a node, ADOPT its shard on a live survivor via
// `Engine::adopt_shards`, and prove its orphaned workflow genuinely replays
// from haematite history and completes.
// ===========================================================================

#[test]
fn ss5_live_survivor_adopts_dead_node_shard_and_resumes_its_workflow() -> TestResult {
    // ONE long-lived runtime drives EVERY async engine call (its worker threads
    // outlive each `block_on`); the blocking haematite elections run off-runtime
    // inside the store seam. See the module docs for the full rationale.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let dirs: Vec<_> = (0..3)
        .map(|_| tempfile::tempdir())
        .collect::<Result<_, _>>()?;

    println!("\n==================================================================");
    println!(" SS-5: MULTI-NODE FAILOVER via Engine::adopt_shards (live survivor)");
    println!("==================================================================");
    println!(" nodes  : ss5-node-0/1/2   |   shard_count = 3");
    println!(" layout : node i owns shard i; node 1's workflow is left IN-FLIGHT");
    println!(" gate   : KILL node 1 -> node 0 (LIVE engine) adopt_shards([1]) ->");
    println!("          node 1's orphaned workflow REPLAYS from haematite + completes;");
    println!("          node 2 (witness) finishes its own workflow uninterrupted.");

    // Workflow CODE is built ONCE and deployed to EVERY node's engine — packages
    // are node-local; only durable STATE replicates (the established workaround).
    let package = example_build::built_package("examples/hello-world", "hello_world")?;
    println!("\n hello_world package built once; loaded on every node's engine.");

    // Bring up the 3-node mesh; link all pairs.
    let send_targets: Vec<Vec<&str>> = (0..3)
        .map(|i| (0..3).filter(|&j| j != i).map(|j| NODE_NAMES[j]).collect())
        .collect();
    let mut nodes: Vec<Node> = (0..3)
        .map(|i| {
            Node::spawn(
                NODE_NAMES[i],
                dirs[i].path(),
                3,
                &send_targets[i],
                SHARD_COUNT,
            )
        })
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
    // ACT 1 — each node owns its shard and seeds its workflow's WorkflowStarted.
    //
    // The deterministic store-append start path is used because
    // `Engine::start_workflow` mints its own id and can't be steered onto a
    // chosen shard. The store must be the live shard owner before the replicated
    // append, so each node manually `acquire_shard_and_serve`s its shard, scopes
    // to it, then records the start. Nodes 0 and 2 then build PRODUCTION engines
    // (the builder's `.owned_shards([i])` re-elects idempotently before recovery
    // re-spawns the workflow). Node 1 deliberately builds NO engine — w[1] stays
    // IN-FLIGHT (only WorkflowStarted durable), modelling a crash the instant
    // after the start was recorded. That makes the ACT 4 adoption the STRONG
    // proof: node 0 must RECOVER and DRIVE an unfinished workflow, not re-serve a
    // completed one.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 1: each node i owns shard i and seeds its workflow ---");
    let workflow_ids: Vec<WorkflowId> = (0..3)
        .map(|i| workflow_id_for_shard(&nodes[i].store, i))
        .collect();
    let run_ids: Vec<RunId> = (0..3).map(|_| RunId::new_v4()).collect();
    let names = ["Shard0", "Shard1", "Shard2"];

    let mut engines: Vec<Option<aion::Engine>> = (0..3).map(|_| None).collect();
    for i in 0..3 {
        engines[i] = seed_node(&SeedInputs {
            runtime: &runtime,
            node: &nodes[i],
            send_targets: &send_targets[i],
            package: &package,
            workflow_id: &workflow_ids[i],
            run_id: &run_ids[i],
            name: names[i],
            shard: i,
            coord_shard,
        })?;
    }

    // ----------------------------------------------------------------------
    // ACT 2 — node 0 completes its OWN workflow (the cluster is serving). w[2] is
    // held IN-FLIGHT across the kill (the across-kill witness).
    // ----------------------------------------------------------------------
    println!("\n--- ACT 2: node 0 completes its OWN workflow ---");
    complete_own_workflow(
        &runtime,
        engine_ref(&engines, 0)?,
        &workflow_ids[0],
        &run_ids[0],
        "Hello, Shard0! Welcome to Aion.",
    )?;
    println!("  (w[1] in-flight -> ADOPTED in ACT 4; w[2] in-flight -> witness across the kill.)");

    // ----------------------------------------------------------------------
    // ACT 3 — KILL node 1. It had no engine; stop its responder + exclude it.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 3: KILL node 1 (owner of shard 1) ---");
    nodes[1].kill();
    println!("  node 1's responder stopped/joined; node 1 is DEAD (w[1] was mid-flight).");
    println!("  the only live copies of shard 1 are now on {{0, 2}}.");

    // ----------------------------------------------------------------------
    // ACT 4 — node 0's LIVE engine ADOPTS shard 1 via the production failover
    // entry point. NO engine rebuild: the same running engine elects shard 1
    // (quorum over the 2 survivors of its 3-node membership — node 1 won't ack,
    // node 0+node 2 = 2 = quorum), union-merges shard 1's committed history, and
    // re-residents w[1] through the production recovery seam.
    //
    // Manual-triggered failover: the test (standing in for a cluster supervisor)
    // detects node 1 gone and EXPLICITLY calls adopt_shards. Automatic detection
    // is SS-5b, deferred.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 4: node 0 LIVE engine adopt_shards([1]) (no rebuild) ---");
    runtime.block_on(engine_ref(&engines, 0)?.adopt_shards(&[1]))?;
    println!("  node 0 elected shard 1 over the survivors, merged it, and re-residented w[1]");
    println!("  on its ORIGINAL, never-rebuilt engine.");

    // ----------------------------------------------------------------------
    // ACT 5 — PROVE the gate. Every assertion lives in `prove_ss5_gate`.
    // ----------------------------------------------------------------------
    println!("\n--- ACT 5: prove the SS-5 failover gate ---");
    let store_0: Arc<dyn EventStore> = Arc::clone(&nodes[0].store) as Arc<dyn EventStore>;
    prove_ss5_gate(&GateInputs {
        runtime: &runtime,
        engine_0: engine_ref(&engines, 0)?,
        witness_engine: engine_ref(&engines, 2)?,
        store_0: &store_0,
        workflow_ids: &workflow_ids,
        run_ids: &run_ids,
    })?;

    print_result_banner();

    // Tear down the live engines; keep the runtime alive until the very end so
    // engine-owned tasks remain valid through shutdown.
    for index in [0, 2] {
        engines
            .get_mut(index)
            .and_then(Option::take)
            .ok_or_else(|| format!("node {index} engine is absent at teardown"))?
            .shutdown()?;
    }
    drop(runtime);
    Ok(())
}
