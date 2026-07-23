//! Task #119 (DISTRIBUTED): a PARKED durable-timer workflow must resume on a
//! survivor that ADOPTS its shard live over a REAL beamr-loopback haematite
//! cluster — the configuration the single-process variant cannot exercise.
//!
//! `ss5_failover_demo` proves the IN-FLIGHT path (only `WorkflowStarted`
//! durable). This proves the harder PARKED-ON-DURABLE-TIMER path across a true
//! cross-node failover: the timer is replicated, the owner dies, and a live
//! survivor adopts the shard and must RE-ARM the parked timer so it fires and
//! the workflow completes exactly once.
//!
//! Harness mirrors `ss5_failover_demo` (one long-lived runtime + `block_on`; the
//! blocking elections run off-runtime in the store seam). THREE nodes (so the two
//! survivors form an election quorum after the kill), three shards. A owns all
//! shards and runs the live sleeper to a PARKED state; B owns a non-sleeper shard
//! and, after A dies, ADOPTS the sleeper's shard (quorum over {B, C}); C is the
//! witness that keeps quorum reachable.

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
use aion_core::{Event, Payload, WorkflowId, WorkflowStatus};
use aion_store::EventStore;
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};
use serde_json::json;

type TestResult = Result<(), Box<dyn Error>>;

const NODE_NAMES: [&str; 3] = [
    "pt-node-0@127.0.0.1",
    "pt-node-1@127.0.0.1",
    "pt-node-2@127.0.0.1",
];
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);
const SHARD_COUNT: usize = 3;
const TOTAL_NODES: usize = 3;
/// Long enough that the sleeper is reliably still parked when A dies, short
/// enough that the re-armed wheel on B fires it within the deadline.
const SLEEP_MS: u64 = 2_000;
const COMPLETE_DEADLINE: Duration = Duration::from_secs(30);

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

/// Spin until the workflow's history (read off `store`) holds `TimerStarted`
/// without a terminal — it has parked on the durable sleep.
fn wait_until_parked(
    runtime: &tokio::runtime::Runtime,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, Box<dyn Error>> {
    let deadline = Instant::now() + COMPLETE_DEADLINE;
    loop {
        let history = runtime.block_on(store.read_history(workflow_id))?;
        let started = history
            .iter()
            .any(|e| matches!(e, Event::TimerStarted { .. }));
        let fired = history
            .iter()
            .any(|e| matches!(e, Event::TimerFired { .. }));
        if started && !fired {
            return Ok(history);
        }
        if Instant::now() >= deadline {
            return Err(format!("workflow never parked on its timer: {history:#?}").into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A spawned, fully-linked three-node cluster and its per-node send-target lists.
struct Cluster {
    nodes: Vec<Node>,
    send_targets: Vec<Vec<&'static str>>,
}

/// Spawn the three-node full-mesh cluster and the per-node send-target lists.
fn spawn_linked_cluster(dirs: &[tempfile::TempDir]) -> Result<Cluster, Box<dyn Error>> {
    let send_targets: Vec<Vec<&str>> = (0..TOTAL_NODES)
        .map(|i| {
            (0..TOTAL_NODES)
                .filter(|&j| j != i)
                .map(|j| NODE_NAMES[j])
                .collect()
        })
        .collect();
    let nodes: Vec<Node> = (0..TOTAL_NODES)
        .map(|i| {
            Node::spawn(
                NODE_NAMES[i],
                dirs[i].path(),
                TOTAL_NODES,
                &send_targets[i],
                SHARD_COUNT,
            )
        })
        .collect::<Result<_, _>>()?;
    link_both(&nodes[0], &nodes[1])?;
    link_both(&nodes[0], &nodes[2])?;
    link_both(&nodes[1], &nodes[2])?;
    Ok(Cluster {
        nodes,
        send_targets,
    })
}

/// One workflow run, plus its parked-state history snapshot, after A drove it.
struct ParkedSleeper {
    engine_a: aion::Engine,
    workflow_id: WorkflowId,
    run_id: aion_core::RunId,
    sleeper_shard: usize,
    parked: Vec<Event>,
}

/// A owns every shard, builds a live engine, starts the sleeper, and waits until
/// it parks on its durable timer (replicated to the quorum).
fn park_sleeper_on_a(
    runtime: &tokio::runtime::Runtime,
    node_a: &Node,
    send_targets_a: &[&str],
    package: &aion_package::Package,
) -> Result<ParkedSleeper, Box<dyn Error>> {
    for shard in 0..SHARD_COUNT {
        node_a.database().acquire_shard_and_serve(
            shard,
            &membership(TOTAL_NODES, send_targets_a),
            OP_TIMEOUT,
        )?;
    }
    let store_a: Arc<dyn EventStore> = Arc::clone(&node_a.store) as Arc<dyn EventStore>;
    let engine_a = runtime.block_on(
        EngineBuilder::new()
            .store_arc(Arc::clone(&store_a))
            .in_memory_visibility()
            .scheduler_threads(1)
            .owned_shards(0..SHARD_COUNT)
            .load_workflows(package.clone())
            .build(),
    )?;

    let input = Payload::from_json(&json!({ "sleep_ms": SLEEP_MS }))?;
    let handle = runtime.block_on(engine_a.start_workflow(
        "sleep_query",
        input,
        std::collections::HashMap::new(),
        String::from("default"),
    ))?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();
    let parked = wait_until_parked(runtime, &store_a, &workflow_id)?;
    let sleeper_shard = node_a.store.shard_for_workflow(&workflow_id);
    Ok(ParkedSleeper {
        engine_a,
        workflow_id,
        run_id,
        sleeper_shard,
        parked,
    })
}

/// Assert the adopted parked workflow re-armed its timer, fired it once, and
/// completed exactly once with an intact history on B.
fn assert_resumed_on_b(
    runtime: &tokio::runtime::Runtime,
    engine_b: &aion::Engine,
    store_b: &Arc<dyn EventStore>,
    sleeper: &ParkedSleeper,
) -> TestResult {
    let result = runtime
        .block_on(async {
            tokio::time::timeout(
                COMPLETE_DEADLINE,
                engine_b.result(&sleeper.workflow_id, &sleeper.run_id),
            )
            .await
        })
        .map_err(|_| {
            "adopted parked workflow never completed: its durable timer was not re-armed on adoption"
        })??;
    let payload = result.map_err(|error| format!("adopted workflow failed on B: {error:?}"))?;
    let output: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(output, json!("slept"));

    let final_history = runtime.block_on(store_b.read_history(&sleeper.workflow_id))?;
    assert_eq!(
        final_history
            .iter()
            .filter(|e| matches!(e, Event::TimerFired { .. }))
            .count(),
        1,
        "the durable timer must fire exactly once: {final_history:#?}"
    );
    assert_eq!(
        final_history
            .iter()
            .filter(|e| matches!(e, Event::WorkflowCompleted { .. }))
            .count(),
        1,
        "the workflow must complete exactly once: {final_history:#?}"
    );
    assert_eq!(
        aion_core::status_from_events(&final_history),
        WorkflowStatus::Completed
    );
    assert_eq!(
        &final_history[..sleeper.parked.len()],
        sleeper.parked.as_slice(),
        "the re-armed resume must extend, never rewrite, the recorded history"
    );
    Ok(())
}

#[test]
fn parked_timer_workflow_resumes_on_xnode_shard_adoption() -> TestResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let dirs: Vec<_> = (0..TOTAL_NODES)
        .map(|_| tempfile::tempdir())
        .collect::<Result<_, _>>()?;
    let package =
        example_build::built_package("crates/aion/tests/fixtures/sleep_query", "sleep_query")?;
    let Cluster {
        mut nodes,
        send_targets,
    } = spawn_linked_cluster(&dirs)?;

    // --- A owns ALL shards and runs the live sleeper to a PARKED state. ---
    let sleeper = park_sleeper_on_a(&runtime, &nodes[0], &send_targets[0], &package)?;

    // --- B boots owning ONLY a non-sleeper shard, so its boot recovery never
    //     re-arms the parked timer. ---
    let other_shard = (0..SHARD_COUNT)
        .find(|&s| s != sleeper.sleeper_shard)
        .ok_or("no non-sleeper shard")?;
    nodes[1].database().acquire_shard_and_serve(
        other_shard,
        &membership(TOTAL_NODES, &send_targets[1]),
        OP_TIMEOUT,
    )?;
    let store_b: Arc<dyn EventStore> = Arc::clone(&nodes[1].store) as Arc<dyn EventStore>;
    let engine_b = runtime.block_on(
        EngineBuilder::new()
            .store_arc(Arc::clone(&store_b))
            .in_memory_visibility()
            .scheduler_threads(1)
            .bootstrap_schedule_coordinator(false)
            .owned_shards([other_shard])
            .load_workflows(package.clone())
            .build(),
    )?;
    assert!(
        engine_b
            .registry()
            .get(&sleeper.workflow_id, &sleeper.run_id)?
            .is_none(),
        "parked workflow is out of B's boot scope; it must NOT be resident yet"
    );

    // --- KILL A. Survivors {B, C} still form an election quorum for adoption. ---
    sleeper.engine_a.shutdown()?;
    nodes[0].kill();

    // --- B ADOPTS the sleeper's shard via the production failover entry point.
    //     Election quorum is reached over the survivors (C's responder acks). ---
    runtime.block_on(engine_b.adopt_shards(&[sleeper.sleeper_shard]))?;

    assert_resumed_on_b(&runtime, &engine_b, &store_b, &sleeper)?;

    engine_b.shutdown()?;
    drop(runtime);
    Ok(())
}
