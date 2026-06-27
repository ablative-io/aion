//! SS-5b: AUTOMATIC multi-node failover detection, proven end-to-end over a real
//! 3-node loopback haematite cluster.
//!
//! This is the auto-detection counterpart to the aion-rs SS-5 demo
//! (`crates/aion/tests/ss5_failover_demo.rs`). SS-5 KILLED a node and then
//! EXPLICITLY called `node0.adopt_shards(&[1])` — a manual trigger. Here there is
//! NO manual adopt call: a [`ClusterSupervisor`] watches node 1's replication
//! liveness, and when node 1 dies (its responder stops, its socket EOFs, the
//! survivors' `is_connected(node-1)` flips to `false`) the supervisor's own poll
//! loop debounces the loss and calls `adopt_shards` ITSELF, driving node 1's
//! orphaned in-flight workflow to completion on node 0's live engine.
//!
//! The supervisor is driven through its public `tick()` (one poll cycle) rather
//! than a background timer, so the test synchronizes on GENUINE conditions — the
//! real link-liveness flip and the real debounce count — with no arbitrary
//! sleeps. The cluster harness mirrors the SS-5 demo's real-beamr-loopback setup.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p aion-server --features haematite-backend \
//!     --test ss5b_auto_failover -- --nocapture
//! ```
#![cfg(feature = "haematite-backend")]

// The `hello_world` archive is rebuilt from the committed Gleam source on every
// run, reusing the aion-rs SS-5 demo's shared example-build helper.
#[path = "../../aion/tests/common/example_build.rs"]
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
use aion_server::cluster::{ClusterSupervisor, SupervisorConfig, WatchedPeer};
use aion_store::{EventStore, WriteToken};
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};
use serde_json::json;

type TestResult = Result<(), Box<dyn Error>>;

const NODE_NAMES: [&str; 3] = [
    "ss5b-node-0@127.0.0.1",
    "ss5b-node-1@127.0.0.1",
    "ss5b-node-2@127.0.0.1",
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
        if request.name.as_str() != "greet" {
            return Err(format!("terminal:unknown activity {}", request.name));
        }
        let value: serde_json::Value = serde_json::from_str(request.input.as_str())
            .map_err(|error| format!("terminal:bad input: {error}"))?;
        let who = value["name"].as_str().unwrap_or("stranger");
        Ok(json!({ "greeting": format!("Hello, {who}! Welcome to Aion.") }).to_string())
    }
}

fn loopback() -> Result<SocketAddr, Box<dyn Error>> {
    Ok("127.0.0.1:0".parse()?)
}

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
        send_targets: send_targets
            .iter()
            .map(|name| SyncNodeId::from(*name))
            .collect(),
    }
}

/// One cluster node: a distributed `HaematiteStore` and a responder thread.
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

    /// Stop this node's inbound-write responder and join its thread. Used at
    /// teardown; on its own this does NOT close the node's replication endpoint
    /// (the `Database`/endpoint stay alive in `store`), so it is not a kill.
    fn stop_responder(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.responder.take() {
            drop(handle.join());
        }
    }
}

/// KILL a node for real: stop its responder, then DROP the whole node so its
/// `Database` and bound `DistributionEndpoint` are torn down and its loopback
/// sockets close. The survivors' read loops then EOF and deregister the link,
/// flipping their `peer_connected(name)` to false — the honest death signal the
/// SS-5b supervisor detects. (Closing the endpoint is exactly what a real
/// `kill -9` does to a process's sockets.)
fn kill_node(mut node: Node) {
    node.stop_responder();
    drop(node);
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

fn workflow_id_for_shard(store: &HaematiteStore, shard: usize) -> WorkflowId {
    loop {
        let candidate = WorkflowId::new_v4();
        if store.shard_for_workflow(&candidate) == shard {
            return candidate;
        }
    }
}

fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

/// Build the 3-node mesh and seed it: node i owns shard i; node 1's workflow is
/// left IN-FLIGHT (no engine) so adoption must genuinely REPLAY it. Returns the
/// nodes, node 0's engine, node 2's witness engine, and the workflow/run ids.
struct SeededCluster {
    /// Nodes are held as `Option` so node 1 can be `take`n out and dropped — a
    /// real kill that closes its endpoint — while node 0 and node 2 keep their
    /// stable indices.
    nodes: Vec<Option<Node>>,
    engine_0: aion::Engine,
    engine_2: aion::Engine,
    workflow_ids: Vec<WorkflowId>,
    run_ids: Vec<RunId>,
}

fn seed_cluster(
    runtime: &tokio::runtime::Runtime,
    dirs: &[tempfile::TempDir],
    package: &aion_package::Package,
) -> Result<SeededCluster, Box<dyn Error>> {
    let send_targets: Vec<Vec<&str>> = (0..3)
        .map(|i| (0..3).filter(|&j| j != i).map(|j| NODE_NAMES[j]).collect())
        .collect();
    let nodes: Vec<Node> = (0..3)
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

    let coord_id = aion::schedule_coordinator_workflow_id();
    let coord_shard = nodes[0].database().shard_for(&event_stream_key(&coord_id));

    let workflow_ids: Vec<WorkflowId> = (0..3)
        .map(|i| workflow_id_for_shard(&nodes[i].store, i))
        .collect();
    let run_ids: Vec<RunId> = (0..3).map(|_| RunId::new_v4()).collect();
    let names = ["Shard0", "Shard1", "Shard2"];

    let mut engines: Vec<Option<aion::Engine>> = (0..3).map(|_| None).collect();
    for i in 0..3 {
        nodes[i].database().acquire_shard_and_serve(
            i,
            &membership(3, &send_targets[i]),
            OP_TIMEOUT,
        )?;
        nodes[i].store.set_owned_shards([i]);
        let store: Arc<dyn EventStore> = Arc::clone(&nodes[i].store) as Arc<dyn EventStore>;
        runtime.block_on(record_start(
            &store,
            &workflow_ids[i],
            &run_ids[i],
            names[i],
            package,
        ))?;
        if i == 1 {
            // Node 1 builds NO engine: w[1] stays in-flight (only WorkflowStarted),
            // so adoption must replay it from scratch — the strong proof.
            continue;
        }
        let bootstrap = i == coord_shard;
        engines[i] = Some(runtime.block_on(build_node_engine(
            Arc::clone(&store),
            package.clone(),
            i,
            bootstrap,
        ))?);
    }

    let engine_0 = engines[0].take().ok_or("node 0 engine missing")?;
    let engine_2 = engines[2].take().ok_or("node 2 engine missing")?;
    Ok(SeededCluster {
        nodes: nodes.into_iter().map(Some).collect(),
        engine_0,
        engine_2,
        workflow_ids,
        run_ids,
    })
}

/// Borrow live node `index` from the cluster (panics via typed error if killed).
fn live_node(cluster: &SeededCluster, index: usize) -> Result<&Node, Box<dyn Error>> {
    cluster
        .nodes
        .get(index)
        .and_then(Option::as_ref)
        .ok_or_else(|| format!("node {index} is not live").into())
}

// ===========================================================================
// THE SS-5b GATE — the supervisor auto-detects node 1's death and adopts its
// shard with NO manual adopt call.
// ===========================================================================
#[test]
fn supervisor_auto_adopts_dead_peer_shard_without_manual_trigger() -> TestResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let dirs: Vec<_> = (0..3)
        .map(|_| tempfile::tempdir())
        .collect::<Result<_, _>>()?;

    println!("\n=== SS-5b: AUTOMATIC failover (supervisor-driven, no manual adopt) ===");
    let package = example_build::built_package("examples/hello-world", "hello_world")?;
    let mut cluster = seed_cluster(&runtime, &dirs, &package)?;
    println!("  3-node cluster up; node 1 owns shard 1 with w[1] left IN-FLIGHT (no engine).");

    // Build the supervisor over node 0's live engine + concrete store, watching
    // node 1 (which owns shard 1). confirmations = 2 exercises real debounce.
    let store_0_concrete = Arc::clone(&live_node(&cluster, 0)?.store);
    let mut supervisor = ClusterSupervisor::new(
        Arc::clone(&store_0_concrete),
        Arc::new(cluster.engine_0),
        vec![WatchedPeer {
            name: NODE_NAMES[1].to_owned(),
            owned_shards: vec![1],
        }],
        SupervisorConfig {
            poll_interval: Duration::from_millis(10),
            confirmations: 2,
        },
    );
    assert!(supervisor.watches_any(), "supervisor must watch node 1");

    // While node 1 is alive, the supervisor must NOT adopt (genuine liveness).
    assert!(
        store_0_concrete.peer_connected(NODE_NAMES[1]),
        "node 0 must see node 1 connected before the kill"
    );
    let adopted = runtime.block_on(supervisor.tick());
    assert!(adopted.is_empty(), "no adoption while node 1 is alive");
    println!("  supervisor tick while node 1 ALIVE -> no adoption (correct).");

    // KILL node 1 FOR REAL: take it out of the cluster and drop it, closing its
    // endpoint. The survivors' links to it EOF; wait for the GENUINE liveness
    // flip (not a fixed sleep) before driving the supervisor.
    println!("  >>> killing node 1 (drop store+endpoint -> socket close) <<<");
    let dead_node = cluster.nodes[1].take().ok_or("node 1 already gone")?;
    kill_node(dead_node);
    assert!(
        wait_until(Duration::from_secs(10), || {
            !store_0_concrete.peer_connected(NODE_NAMES[1])
        }),
        "node 0 must observe node 1's link drop after the kill"
    );
    println!("  node 0 observed node 1's replication link DROP (peer_connected -> false).");

    // Drive supervisor ticks. Debounce = 2, so the FIRST down-tick must not adopt;
    // the SECOND must. This proves debounce over the real liveness signal.
    let first = runtime.block_on(supervisor.tick());
    assert!(
        first.is_empty(),
        "debounce: first down-tick must not adopt yet"
    );
    let second = runtime.block_on(supervisor.tick());
    assert_eq!(
        second,
        vec![NODE_NAMES[1].to_owned()],
        "second consecutive down-tick must AUTO-adopt node 1's shard"
    );
    println!("  supervisor AUTO-adopted shard 1 on the 2nd consecutive down-tick (debounced).");

    // PROVE the resume is genuine: w[1] (only ever a WorkflowStarted on the dead
    // node) now replays from the union-merged haematite history on node 0 and
    // completes — no manual adopt was ever called.
    let store_0: Arc<dyn EventStore> = Arc::clone(&store_0_concrete) as Arc<dyn EventStore>;
    let result = runtime.block_on(
        supervisor_engine(&supervisor).result(&cluster.workflow_ids[1], &cluster.run_ids[1]),
    )?;
    let payload = result.map_err(|error| format!("auto-adopted w[1] failed: {error:?}"))?;
    let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    println!("  w[1] COMPLETED on node 0 after AUTO-failover -> {greeting}");
    assert_eq!(greeting, json!("Hello, Shard1! Welcome to Aion."));

    // History intact: one WorkflowStarted, contiguous seqs, terminal Completed.
    let history = runtime.block_on(store_0.read_history(&cluster.workflow_ids[1]))?;
    assert_eq!(
        history
            .iter()
            .filter(|e| matches!(e, Event::WorkflowStarted { .. }))
            .count(),
        1,
        "auto-adopt must not duplicate w[1]'s WorkflowStarted"
    );
    for (index, event) in history.iter().enumerate() {
        assert_eq!(
            event.seq(),
            u64::try_from(index + 1)?,
            "contiguous seqs (no lost events)"
        );
    }
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed,
        "w[1] must be terminally Completed"
    );
    println!("  w[1] history intact: one start, contiguous, terminal Completed.");

    // Witness: node 2 finishes its OWN workflow uninterrupted across the kill.
    let witness = runtime.block_on(
        cluster
            .engine_2
            .result(&cluster.workflow_ids[2], &cluster.run_ids[2]),
    )?;
    let witness_payload = witness.map_err(|error| format!("witness w[2] failed: {error:?}"))?;
    let witness_greeting: serde_json::Value = serde_json::from_slice(witness_payload.bytes())?;
    assert_eq!(witness_greeting, json!("Hello, Shard2! Welcome to Aion."));
    println!("  witness node 2 finished its own w[2] uninterrupted -> {witness_greeting}");

    println!("=== SS-5b PROVED: supervisor auto-failover, no manual adopt ===\n");

    // Tear down: recover the engines from the supervisor and node 2, shut down.
    cluster.engine_2.shutdown()?;
    supervisor_engine(&supervisor).shutdown()?;
    drop(runtime);
    Ok(())
}

/// Borrow the engine the supervisor drives (its adopter), for the post-adopt
/// result/shutdown assertions. Kept as a tiny helper so the test reads cleanly.
fn supervisor_engine(
    supervisor: &ClusterSupervisor<HaematiteStore, aion::Engine>,
) -> &aion::Engine {
    supervisor.adopter()
}
