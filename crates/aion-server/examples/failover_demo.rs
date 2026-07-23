//! A RUNNABLE, watchable multi-node failover demo for Aion-on-haematite.
//!
//! Boots a small Aion cluster on the haematite backend, starts a workflow whose
//! owner node then DIES, and watches a surviving node's SS-5b cluster supervisor
//! AUTOMATICALLY detect the death and pick the workflow up — driving it to
//! completion with narrated output. No human triggers the failover; the
//! supervisor does, exactly as it would in the running `aion` server.
//!
//! ## Run it
//!
//! ```text
//! cargo run -p aion-server --example failover_demo --features haematite-backend
//! ```
//!
//! (`gleam` must be on PATH — the `hello-world` workflow is rebuilt from source.)
//!
//! ## In-process, and HONEST about it
//!
//! This demo runs N Aion engines as N nodes inside ONE OS process, over a REAL
//! beamr loopback haematite cluster: genuine bound replication endpoints, genuine
//! quorum-replicated writes, genuine per-shard election + `become_live`
//! union-merge, and the genuine production `Engine::adopt_shards` failover path
//! driven by the genuine `ClusterSupervisor`. Node death is modelled by DROPPING
//! the dead node's store + endpoint, which closes its loopback sockets — the same
//! thing a real `kill -9` does to a process's sockets, so the survivor's
//! `peer_connected` liveness signal flips for real. The ONE thing it is not is a
//! separate OS process per node; see the report for why (the `aion` binary's
//! activities run over the remote-worker protocol, out of scope for this demo).
#![cfg(feature = "haematite-backend")]

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
use aion_package::{PackageOptions, package_project};
use aion_server::cluster::{ClusterSupervisor, SupervisorConfig, WatchedPeer};
use aion_store::{EventStore, WriteToken};
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};
use serde_json::json;

type DemoResult<T> = Result<T, Box<dyn Error>>;

const NODE_NAMES: [&str; 3] = [
    "demo-node-0@127.0.0.1",
    "demo-node-1@127.0.0.1",
    "demo-node-2@127.0.0.1",
];
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);
const SHARD_COUNT: usize = 3;

fn main() -> DemoResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    banner();

    let package = build_hello_world()?;
    println!("  [build] hello_world workflow built from source, loaded on every node.\n");

    let dirs: Vec<_> = (0..3)
        .map(|_| tempfile::tempdir())
        .collect::<Result<_, _>>()?;
    let mut cluster = Cluster::boot(&runtime, &dirs, &package)?;

    // --- The supervisor on node 0 watches node 1 (owner of shard 1). ---
    let store_0 = Arc::clone(&cluster.live(0)?.store);
    let engine_0 = cluster.take_engine(0)?;
    let mut supervisor = ClusterSupervisor::new(
        Arc::clone(&store_0),
        Arc::new(engine_0),
        vec![WatchedPeer {
            name: NODE_NAMES[1].to_owned(),
            owned_shards: vec![1],
        }],
        SupervisorConfig {
            poll_interval: Duration::from_millis(50),
            confirmations: 3,
        },
    );
    println!(
        "  [supervisor] node-0 watching node-1 (owner of shard 1); \
         debounce = 3 polls @ 50ms.\n"
    );

    let w1 = cluster.workflow_ids[1].clone();
    let r1 = cluster.run_ids[1].clone();

    act_running_cluster(&runtime, &cluster, &mut supervisor)?;
    act_kill_owner(&mut cluster, &store_0)?;
    act_auto_failover(&runtime, &mut supervisor, &store_0, &w1, &r1)?;
    act_witness(&runtime, &cluster)?;

    closing_banner();

    cluster.shutdown_witness()?;
    supervisor.adopter().shutdown()?;
    drop(runtime);
    Ok(())
}

/// ACT 1: the cluster is serving. Node 0 completes its own workflow, and a
/// supervisor tick confirms node 1 is alive so nothing is adopted.
fn act_running_cluster(
    runtime: &tokio::runtime::Runtime,
    cluster: &Cluster,
    supervisor: &mut ClusterSupervisor<HaematiteStore, aion::Engine>,
) -> DemoResult<()> {
    println!("--- ACT 1: the cluster is up and SERVING ---");
    println!("  node-0 OWNS shard 0, running w[0]");
    println!("  node-1 OWNS shard 1, running w[1]   (left IN-FLIGHT to prove a real resume)");
    println!("  node-2 OWNS shard 2, running w[2]");
    let greeting = runtime.block_on(
        supervisor
            .adopter()
            .result(&cluster.workflow_ids[0], &cluster.run_ids[0]),
    )?;
    let greeting = greeting.map_err(|error| format!("w[0] failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(greeting.bytes())?;
    println!("  node-0: w[0] completed -> {value}");
    let adopted = runtime.block_on(supervisor.tick());
    println!(
        "  supervisor poll: node-1 connected = {} -> adopted {:?} (nothing, correct)\n",
        store_sees_peer(cluster, 0, 1),
        adopted
    );
    Ok(())
}

/// ACT 2: kill node 1 for real (drop its store + endpoint -> sockets close).
fn act_kill_owner(cluster: &mut Cluster, store_0: &Arc<HaematiteStore>) -> DemoResult<()> {
    println!("--- ACT 2: the owner DIES ---");
    println!("  \u{1f480} kill node-1  (dropping its store + replication endpoint)");
    let dead = cluster.nodes[1].take().ok_or("node-1 already dead")?;
    dead.kill();
    let observed = wait_until(Duration::from_secs(10), || {
        !store_0.peer_connected(NODE_NAMES[1])
    });
    println!(
        "  node-0 detected node-1's replication link DROP: peer_connected(node-1) = {}\n",
        if observed { "false" } else { "STILL TRUE (!)" }
    );
    Ok(())
}

/// ACT 3: the supervisor auto-detects + debounces + adopts, and the orphaned
/// workflow replays to completion. NO manual adopt call anywhere.
fn act_auto_failover(
    runtime: &tokio::runtime::Runtime,
    supervisor: &mut ClusterSupervisor<HaematiteStore, aion::Engine>,
    store_0: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> DemoResult<()> {
    println!("--- ACT 3: AUTOMATIC failover (no human, no manual adopt) ---");
    // Drive the supervisor's own poll loop until it adopts, exactly as the
    // background task does in the server — synchronizing on the genuine result.
    let mut poll = 0u32;
    let adopted = loop {
        poll += 1;
        let adopted = runtime.block_on(supervisor.tick());
        if adopted.is_empty() {
            println!("  poll {poll}: node-1 still down, debouncing... (not adopting yet)");
            continue;
        }
        break adopted;
    };
    println!("  poll {poll}: node-0 detected node-1 DOWN past debounce -> adopting shard 1");
    println!("            (elect shard 1 over the survivors, union-merge its history, resume)");
    println!("  \u{2705} node-0 adopted {adopted:?}");

    // The orphaned in-flight workflow now replays on node-0 and completes.
    let result = runtime.block_on(supervisor.adopter().result(workflow_id, run_id))?;
    let payload = result.map_err(|error| format!("adopted workflow failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    println!("  recovered w[1] -> \u{2705} completed: {value}");

    let history = assert_resumed(runtime, store_0, workflow_id)?;
    let starts = history
        .iter()
        .filter(|e| matches!(e, Event::WorkflowStarted { .. }))
        .count();
    println!(
        "  history intact on node-0: {} events, {starts} WorkflowStarted, terminal Completed\n",
        history.len()
    );
    Ok(())
}

/// ACT 4: the witness (node 2) finished its own workflow uninterrupted.
fn act_witness(runtime: &tokio::runtime::Runtime, cluster: &Cluster) -> DemoResult<()> {
    println!("--- ACT 4: the witness is unaffected ---");
    let witness = cluster.witness_engine.as_ref().ok_or("no witness engine")?;
    let result = runtime.block_on(witness.result(&cluster.workflow_ids[2], &cluster.run_ids[2]))?;
    let payload = result.map_err(|error| format!("witness w[2] failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    println!("  node-2: w[2] completed on its ORIGINAL engine, across the kill -> {value}\n");
    Ok(())
}

// The helper functions referenced above that needed real plumbing are defined
// below; the demo's load-bearing assertions live here so the narration stays
// honest (it only prints what actually happened).

fn store_sees_peer(cluster: &Cluster, from: usize, peer: usize) -> bool {
    cluster
        .live(from)
        .is_ok_and(|node| node.store.peer_connected(NODE_NAMES[peer]))
}

fn banner() {
    println!("\n==================================================================");
    println!(" Aion-on-haematite  |  AUTOMATIC multi-node failover (SS-5 + SS-5b)");
    println!("==================================================================");
    println!(" in-process, real loopback haematite cluster (see file header)\n");
}

fn closing_banner() {
    println!("==================================================================");
    println!(" RESULT: node-1 died; node-0's cluster supervisor detected it,");
    println!("         debounced, AUTO-adopted shard 1, and node-1's orphaned");
    println!("         workflow replayed from haematite history to completion.");
    println!("         node-2 finished uninterrupted. Durable auto-failover works.");
    println!("==================================================================\n");
}

// ---------------------------------------------------------------------------
// Cluster harness (real beamr loopback haematite).
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

struct Node {
    store: Arc<HaematiteStore>,
    event_store: Arc<haematite::EventStore>,
    addr: SocketAddr,
    name: &'static str,
    responder: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Node {
    fn spawn(name: &'static str, dir: &Path, send_targets: &[&str]) -> DemoResult<Self> {
        let endpoint = DistributionEndpoint::bind(name, "127.0.0.1:0".parse()?, 1, None)?;
        let addr = endpoint.local_addr();
        let database = Database::create(DatabaseConfig {
            data_dir: dir.join("db"),
            shard_count: SHARD_COUNT,
            executor_threads: None,
            distributed: None,
        })?
        .with_distribution(endpoint);
        let store = Arc::new(HaematiteStore::with_distribution(
            database,
            WriteMembership {
                total_nodes: 3,
                send_targets: send_targets.iter().map(|n| SyncNodeId::from(*n)).collect(),
            },
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

    /// Kill this node: stop its responder, then drop it (closing its endpoint).
    fn kill(mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.responder.take() {
            drop(handle.join());
        }
        drop(self);
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

struct Cluster {
    nodes: Vec<Option<Node>>,
    engines: Vec<Option<aion::Engine>>,
    witness_engine: Option<aion::Engine>,
    workflow_ids: Vec<WorkflowId>,
    run_ids: Vec<RunId>,
}

impl Cluster {
    fn boot(
        runtime: &tokio::runtime::Runtime,
        dirs: &[tempfile::TempDir],
        package: &aion_package::Package,
    ) -> DemoResult<Self> {
        let send_targets: Vec<Vec<&str>> = (0..3)
            .map(|i| (0..3).filter(|&j| j != i).map(|j| NODE_NAMES[j]).collect())
            .collect();
        let nodes: Vec<Node> = (0..3)
            .map(|i| Node::spawn(NODE_NAMES[i], dirs[i].path(), &send_targets[i]))
            .collect::<Result<_, _>>()?;
        for (a, b) in [(0, 1), (0, 2), (1, 2)] {
            link_both(&nodes[a], &nodes[b])?;
        }

        let coord_shard = nodes[0]
            .database()
            .shard_for(&event_stream_key(&aion::schedule_coordinator_workflow_id()));
        let workflow_ids: Vec<WorkflowId> = (0..3)
            .map(|i| workflow_id_for_shard(&nodes[i].store, i))
            .collect();
        let run_ids: Vec<RunId> = (0..3).map(|_| RunId::new_v4()).collect();
        let names = ["Shard0", "Shard1", "Shard2"];

        let mut engines: Vec<Option<aion::Engine>> = (0..3).map(|_| None).collect();
        for i in 0..3 {
            nodes[i].database().acquire_shard_and_serve(
                i,
                &WriteMembership {
                    total_nodes: 3,
                    send_targets: send_targets[i]
                        .iter()
                        .map(|n| SyncNodeId::from(*n))
                        .collect(),
                },
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
                continue; // in-flight, no engine
            }
            engines[i] = Some(runtime.block_on(build_engine(
                Arc::clone(&store),
                package.clone(),
                i,
                i == coord_shard,
            ))?);
        }
        let witness_engine = engines[2].take();
        Ok(Self {
            nodes: nodes.into_iter().map(Some).collect(),
            engines,
            witness_engine,
            workflow_ids,
            run_ids,
        })
    }

    fn live(&self, index: usize) -> DemoResult<&Node> {
        self.nodes
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| format!("node {index} is not live").into())
    }

    fn take_engine(&mut self, index: usize) -> DemoResult<aion::Engine> {
        self.engines
            .get_mut(index)
            .and_then(Option::take)
            .ok_or_else(|| format!("node {index} engine missing").into())
    }

    fn shutdown_witness(&mut self) -> DemoResult<()> {
        if let Some(engine) = self.witness_engine.take() {
            engine.shutdown()?;
        }
        Ok(())
    }
}

fn link(from: &Node, to: &Node) -> DemoResult<()> {
    let endpoint = from
        .database()
        .distribution()
        .ok_or("dialing node has no endpoint")?;
    endpoint.add_peer(to.name, to.addr);
    endpoint.connect(to.name)?;
    if !wait_until(HANDSHAKE_TIMEOUT, || endpoint.is_connected(to.name)) {
        return Err(format!("{} never linked to {}", from.name, to.name).into());
    }
    Ok(())
}

fn link_both(a: &Node, b: &Node) -> DemoResult<()> {
    link(a, b)?;
    link(b, a)?;
    Ok(())
}

async fn build_engine(
    store: Arc<dyn EventStore>,
    package: aion_package::Package,
    owned_shard: usize,
    bootstrap_coordinator: bool,
) -> DemoResult<aion::Engine> {
    Ok(EngineBuilder::new()
        .store_arc(store)
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .bootstrap_schedule_coordinator(bootstrap_coordinator)
        .owned_shards([owned_shard])
        .load_workflows(package)
        .build()
        .await?)
}

async fn record_start(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    name: &str,
    package: &aion_package::Package,
) -> DemoResult<()> {
    let start = Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq: 1,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: String::from("hello_world"),
        input: Payload::from_json(&json!({ "name": name }))?,
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

fn build_hello_world() -> DemoResult<aion_package::Package> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .ancestors()
        .nth(2)
        .ok_or("cannot locate repo root")?
        .join("examples/hello-world");
    let status = std::process::Command::new("gleam")
        .arg("build")
        .current_dir(&root)
        .status()
        .map_err(|error| {
            format!("`gleam build` could not be spawned (is gleam on PATH?): {error}")
        })?;
    if !status.success() {
        return Err(format!("`gleam build` failed in {}", root.display()).into());
    }
    let report = package_project(&root, &PackageOptions::default())?;
    report
        .packages
        .iter()
        .find(|packaged| packaged.workflow_type == "hello_world")
        .map(|packaged| packaged.package.clone())
        .ok_or_else(|| "hello-world does not declare workflow type hello_world".into())
}

/// Verify, then assert, the auto-failover outcome. Returns the orphaned
/// workflow's intact, completed history for the narration.
fn assert_resumed(
    runtime: &tokio::runtime::Runtime,
    store_0: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> DemoResult<Vec<Event>> {
    let store_dyn: Arc<dyn EventStore> = Arc::clone(store_0) as Arc<dyn EventStore>;
    let history = runtime.block_on(store_dyn.read_history(workflow_id))?;
    if aion_core::status_from_events(&history) != WorkflowStatus::Completed {
        return Err("adopted workflow did not reach Completed".into());
    }
    Ok(history)
}
