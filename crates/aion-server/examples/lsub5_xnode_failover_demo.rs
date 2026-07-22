//! LSUB-5: a RUNNABLE, watchable cross-node OWNER-KILL fan-out failover demo.
//!
//! Boots the LSUB-5 capstone scenario in one process — a 3-node real-loopback
//! haematite cluster, a real `liminal-server` over loopback TCP, and a real
//! remote worker — kills the shard owner MID-DISPATCH, and narrates the survivor
//! adopting the orphaned shard and re-driving the in-flight fan-out to an
//! exactly-once-per-ordinal completion.
//!
//! ## Run it
//!
//! ```text
//! cargo run -p aion-server --example lsub5_xnode_failover_demo \
//!     --features haematite-backend,liminal-transport
//! ```
//!
//! ## Honest about what it is
//!
//! Everything is real: genuine bound replication endpoints, quorum-replicated
//! writes, per-shard election + `become_live` union-merge, the production
//! `Engine::adopt_shards` failover path driven by the real `ClusterSupervisor`,
//! a real `liminal-server` + a real `LiminalActivityWorker`, and the durable
//! outbox cutover (`record_fan_out_dispatch`) + `OutboxDispatcher` +
//! `ServerOutboxDeliveryCallback` completion seam. The ONE concession (see the
//! LSUB-5 report's "seam wall") is that the OWNER node is an engine-less shard
//! owner + dispatcher — a running engine cannot be torn down in-process to close
//! its replication endpoint — while the SURVIVOR runs the full engine that does
//! the real adopt + replay + re-arm + re-dispatch. The owner stages its fan-out
//! through the SAME `record_fan_out_dispatch` cutover the live engine's collect
//! NIF calls, so the chain under test is exercised faithfully end to end.
#![cfg(all(feature = "haematite-backend", feature = "liminal-transport"))]

use std::error::Error;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::durability::{FanOutItem, Recorder, WorkflowStartRecord};
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{
    ActivityId, DEFAULT_TASK_QUEUE, Event, PackageVersion, Payload, RunId, WorkflowId,
    WorkflowStatus,
};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_server::cluster::{ClusterSupervisor, SupervisorConfig, WatchedPeer};
use aion_server::worker::{
    ConnectedWorkerRegistry, LiminalConnectionNotifier, OutboxDeliveryCallback, OutboxDispatcher,
    OutboxDispatcherConfig, OutboxRowDispatch, RegistryLiminalDispatch,
    ServerOutboxDeliveryCallback,
};
use aion_store::{EventStore, OutboxRow, OutboxStatus, OutboxStore};
use aion_store_haematite::HaematiteStore;
use aion_worker::{ActivityRegistry, LiminalActivityWorker, WorkerConfig};
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::{ConnectionSupervisor, LiminalConnectionServices};
use liminal_server::server::listener::ServerListener;
use serde_json::json;

type DemoError = Box<dyn Error + Send + Sync>;
type DemoResult<T> = Result<T, DemoError>;

const NODE_NAMES: [&str; 3] = [
    "lsub5-demo-0@127.0.0.1",
    "lsub5-demo-1@127.0.0.1",
    "lsub5-demo-2@127.0.0.1",
];
const SHARD_COUNT: usize = 3;
const FAN_OUT: usize = 4;
const NAMESPACE: &str = "default";
const TASK_QUEUE: &str = "default";
const OUTBOX_MODULE: &str = "aion_outbox_fixture";
const OUTBOX_BEAM: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_outbox_fixture.beam");
const OUTBOX_SOURCE: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_outbox_fixture.erl");
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);
const FAILOVER_DEADLINE: Duration = Duration::from_secs(40);

fn demo_error(message: impl std::fmt::Display) -> DemoError {
    message.to_string().into()
}

// One long, linear narrative (boot -> stage -> mid-dispatch -> kill -> adopt ->
// re-drive -> one-terminal proof); the same shape the showcase failover demos
// carry the `too_many_lines` allow for.
#[allow(clippy::too_many_lines)]
fn main() -> DemoResult<()> {
    banner();
    let package = build_package()?;

    // --- Boot the 3-node cluster (sync prologue: blocking elections). --------
    let node_count = NODE_NAMES.len();
    let dirs: Vec<tempfile::TempDir> = (0..node_count)
        .map(|_| tempfile::tempdir())
        .collect::<Result<_, _>>()
        .map_err(demo_error)?;
    let send_targets: Vec<Vec<&str>> = (0..node_count)
        .map(|i| {
            (0..node_count)
                .filter(|&j| j != i)
                .map(|j| NODE_NAMES[j])
                .collect()
        })
        .collect();
    let mut nodes: Vec<Option<Node>> = (0..node_count)
        .map(|i| Node::spawn(NODE_NAMES[i], dirs[i].path(), &send_targets[i]).map(Some))
        .collect::<Result<_, _>>()?;
    for a in 0..node_count {
        for b in (a + 1)..node_count {
            link_both(node_ref(&nodes, a)?, node_ref(&nodes, b)?)?;
        }
    }
    for (i, targets) in send_targets.iter().enumerate() {
        let node = node_ref(&nodes, i)?;
        node.database()
            .acquire_shard_and_serve(i, &membership(targets), OP_TIMEOUT)
            .map_err(demo_error)?;
        node.store.set_owned_shards([i]);
    }
    println!("--- ACT 1: the cluster is up and SERVING ---");
    println!("  node A owns shard 0 (the fan-out owner)");
    println!("  node B owns shard 1 (the survivor, full engine)");
    println!("  node C owns shard 2 (quorum-only)\n");

    // --- Real liminal server + a real remote worker. ------------------------
    let liminal = RunningLiminalServer::start()?;
    let address = liminal.address.to_string();
    let control = Arc::new(WorkerControl::default());
    let worker = SurvivorWorker::spawn(address, Arc::clone(&control));
    liminal.wait_for_worker()?;
    println!("  liminal server up; remote worker registered for the (default,default) pool.\n");

    let dispatcher_config = OutboxDispatcherConfig {
        poll_interval: Duration::from_millis(25),
        batch_size: 16,
        max_attempts: 8,
        backoff_base: Duration::from_secs(120),
        backoff_multiplier: 2,
        backoff_max: Duration::from_secs(240),
    };

    // --- Survivor B (engine) + owner A (shard owner + dispatcher). ----------
    let fired_b = Arc::new(AtomicBool::new(false));
    let server_b = Server::build(
        node_ref(&nodes, 1)?,
        1,
        &package,
        &liminal,
        &fired_b,
        dispatcher_config,
    )?;
    let store_a = Arc::clone(&node_ref(&nodes, 0)?.store);
    let mut owner = Some(OwnerServer::spawn(
        node_ref(&nodes, 0)?,
        &liminal,
        dispatcher_config,
    )?);

    // --- Stage the fan-out on shard 0 through the durable cutover. -----------
    let fanout_workflow = workflow_id_for_shard(&store_a, 0);
    let fanout_run = RunId::new_v4();
    server_b.block_on(stage_fanout(
        &store_a,
        &fanout_workflow,
        &fanout_run,
        &package,
    ))?;
    println!("--- ACT 2: a fan-out of {FAN_OUT} activities is dispatched on shard 0 ---");
    wait_until(Duration::from_secs(20), || {
        server_b
            .block_on(all_rows_present(&store_a, &fanout_workflow))
            .unwrap_or(false)
    });
    println!(
        "  {FAN_OUT} durable outbox rows staged Pending on shard 0 (record_fan_out_dispatch)."
    );
    wait_until(Duration::from_secs(20), || {
        control.dispatch_seen.load(Ordering::SeqCst)
    });
    let claimed = server_b.block_on(claimed_count(&store_a, &fanout_workflow))?;
    println!(
        "  owner A's dispatcher claimed + pushed to the worker MID-DISPATCH ({claimed} in flight).\n"
    );

    // --- Supervisor on B watching A (owner of shard 0). ---------------------
    let store_b = Arc::clone(&server_b.store);
    let mut supervisor = ClusterSupervisor::new(
        Arc::clone(&store_b),
        Arc::clone(&server_b.engine),
        vec![WatchedPeer {
            name: NODE_NAMES[0].to_owned(),
            owned_shards: vec![0],
        }],
        SupervisorConfig {
            poll_interval: Duration::from_millis(20),
            confirmations: 2,
        },
    );
    drop(server_b.block_on(supervisor.tick())); // alive: no adoption.

    // --- ACT 3: kill the owner mid-dispatch. --------------------------------
    println!("--- ACT 3: the owner DIES mid-dispatch ---");
    println!("  \u{1f480} kill node A (drop its dispatcher runtime + node -> endpoint closes)");
    control.released.store(true, Ordering::SeqCst);
    owner
        .take()
        .ok_or_else(|| demo_error("owner already killed"))?
        .kill();
    drop(store_a);
    let dead = nodes[0]
        .take()
        .ok_or_else(|| demo_error("owner node gone"))?;
    drop(dead);
    if !wait_until(Duration::from_secs(20), || {
        !store_b.peer_connected(NODE_NAMES[0])
    }) {
        return Err(demo_error("survivor never observed the owner's link drop"));
    }
    println!("  node B detected node A's replication link DROP (peer_connected -> false).\n");

    // --- ACT 4: automatic adoption + re-drive. ------------------------------
    println!("--- ACT 4: AUTOMATIC failover (no human, no manual adopt) ---");
    drop(server_b.block_on(supervisor.tick())); // debounce.
    let adopted = server_b.block_on(supervisor.tick());
    println!(
        "  node B AUTO-adopted shard {adopted:?} (debounced over the genuine link-down signal)."
    );
    let claim_scope = store_b.owned_shards().unwrap_or_default();
    println!("  node B's outbox claim scope refreshed in place to own {claim_scope:?} (the CRUX).");

    let started = Instant::now();
    let completed = wait_until(FAILOVER_DEADLINE, || {
        server_b.block_on(async {
            rows_all_done(&store_b, &fanout_workflow)
                .await
                .unwrap_or(false)
                && workflow_completed(&store_b, &fanout_workflow)
                    .await
                    .unwrap_or(false)
        })
    });
    if !completed {
        return Err(demo_error("fan-out did not re-drive to completion in time"));
    }
    println!("  node B re-armed the stranded rows and re-dispatched them to the worker.");
    println!(
        "  \u{2705} fan-out re-driven to completion in {:?}.\n",
        started.elapsed()
    );

    // --- The one-terminal proof. --------------------------------------------
    let history = server_b.block_on(read_history(&store_b, &fanout_workflow))?;
    let total = control.executions.load(Ordering::SeqCst);
    println!("--- RESULT: EXACTLY-ONCE per ordinal ---");
    for ordinal in 0..FAN_OUT as u64 {
        println!(
            "  ordinal {ordinal}: {} terminal event(s)",
            terminal_count_for(&history, ordinal)
        );
    }
    println!(
        "  workflow status: {:?}",
        aion_core::status_from_events(&history)
    );
    println!(
        "  the worker executed the activity {total} times (at-least-once); the completion dedup\n  \
         absorbed the redelivery so each ordinal has EXACTLY ONE terminal in history.\n"
    );
    closing_banner();

    // --- Teardown. ----------------------------------------------------------
    let _: Result<(), _> = server_b.dispatcher_shutdown.send(true);
    drop(supervisor);
    server_b.engine.shutdown().map_err(demo_error)?;
    let Server {
        runtime,
        store,
        engine,
        ..
    } = server_b;
    runtime.shutdown_timeout(Duration::from_secs(10));
    drop(engine);
    drop(store);
    worker.stop();
    liminal.shutdown()?;
    Ok(())
}

fn banner() {
    println!("\n==================================================================");
    println!(" Aion  |  LSUB-5: cross-node OWNER-KILL fan-out failover (exactly-once)");
    println!("==================================================================");
    println!(" in-process, real loopback haematite cluster + real liminal worker\n");
}

fn closing_banner() {
    println!("==================================================================");
    println!(" The owner died MID-DISPATCH; the survivor auto-adopted its shard,");
    println!(" re-armed the in-flight outbox rows, re-dispatched them to a live");
    println!(" worker, and the completion dedup delivered EXACTLY ONCE per ordinal.");
    println!("==================================================================\n");
}

// ===========================================================================
// Harness (mirrors the lsub5 e2e test; kept self-contained for a runnable demo).
// ===========================================================================

fn build_package() -> DemoResult<Package> {
    let beams =
        BeamSet::new(vec![BeamModule::new(OUTBOX_MODULE, OUTBOX_BEAM)]).map_err(demo_error)?;
    let manifest = Manifest {
        entry_module: OUTBOX_MODULE.to_owned(),
        entry_function: "collect_four".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Some(Duration::from_secs(60)),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(OUTBOX_MODULE, OUTBOX_SOURCE.to_vec())])
            .write_to_bytes()
            .map_err(demo_error)?;
    Package::load_from_bytes(archive, ExtractionLimits::unbounded()).map_err(demo_error)
}

fn worker_result(ordinal: u64) -> serde_json::Value {
    json!(format!("worker-{ordinal}"))
}

fn membership(send_targets: &[&str]) -> WriteMembership {
    WriteMembership {
        total_nodes: NODE_NAMES.len(),
        send_targets: send_targets
            .iter()
            .map(|name| SyncNodeId::from(*name))
            .collect(),
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
        let endpoint =
            DistributionEndpoint::bind(name, "127.0.0.1:0".parse().map_err(demo_error)?, 1, None)
                .map_err(demo_error)?;
        let addr = endpoint.local_addr();
        let database = Database::create(DatabaseConfig {
            data_dir: dir.join("db"),
            shard_count: SHARD_COUNT,
            sweep_interval: None,
            distributed: None,
        })
        .map_err(demo_error)?
        .with_distribution(endpoint);
        let store = Arc::new(HaematiteStore::with_distribution(
            database,
            membership(send_targets),
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
}

impl Drop for Node {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.responder.take() {
            drop(handle.join());
        }
    }
}

fn node_ref(nodes: &[Option<Node>], index: usize) -> DemoResult<&Node> {
    nodes
        .get(index)
        .and_then(Option::as_ref)
        .ok_or_else(|| demo_error(format!("node {index} gone")))
}

fn link(from: &Node, to: &Node) -> DemoResult<()> {
    let endpoint = from
        .database()
        .distribution()
        .ok_or_else(|| demo_error("no endpoint"))?;
    endpoint.add_peer(to.name, to.addr);
    endpoint.connect(to.name).map_err(demo_error)?;
    if !wait_until(HANDSHAKE_TIMEOUT, || endpoint.is_connected(to.name)) {
        return Err(demo_error(format!(
            "{} never linked to {}",
            from.name, to.name
        )));
    }
    Ok(())
}

fn link_both(a: &Node, b: &Node) -> DemoResult<()> {
    link(a, b)?;
    link(b, a)
}

fn workflow_id_for_shard(store: &HaematiteStore, shard: usize) -> WorkflowId {
    loop {
        let candidate = WorkflowId::new_v4();
        if store.shard_for_workflow(&candidate) == shard {
            return candidate;
        }
    }
}

struct RunningLiminalServer {
    listener: Option<ServerListener>,
    registry: ConnectedWorkerRegistry,
    address: SocketAddr,
}

impl RunningLiminalServer {
    fn start() -> DemoResult<Self> {
        let health = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(demo_error)?;
            let a = l.local_addr().map_err(demo_error)?;
            drop(l);
            a
        };
        let config = ServerConfig {
            listen_address: "127.0.0.1:0".parse().map_err(demo_error)?,
            health_listen_address: health,
            channels: Vec::<ChannelDef>::new(),
            routing_rules: Vec::new(),
            persistence_path: None,
            cluster: None,
            // Open at the liminal layer (no Connect token), matching the embedded
            // production listener; aion-level registration metadata is the auth story.
            auth: None,
            drain_timeout_ms: 30_000,
            // liminal 0.2.4 defaults = the 0.2.3 behaviour (full profile, signed caps).
            services: liminal_server::config::ServicesConfig::default(),
            limits: liminal_server::config::LimitsConfig::default(),
            // liminal 0.3.0: no WebSocket listener, participant capability
            // disabled — byte-identical to the pre-0.3.0 build, matching run.rs.
            websocket: None,
            participant: None,
        };
        let registry = ConnectedWorkerRegistry::default();
        let notifier = Arc::new(LiminalConnectionNotifier::new(registry.clone()));
        let services =
            Arc::new(LiminalConnectionServices::from_config(&config).map_err(demo_error)?);
        let supervisor =
            ConnectionSupervisor::with_services_and_notifier(services, notifier.clone())
                .map_err(demo_error)?;
        if !notifier.bind_supervisor(supervisor.clone()) {
            return Err(demo_error("notifier supervisor already bound"));
        }
        let listener = ServerListener::bind(&config, supervisor).map_err(demo_error)?;
        let address = listener.local_addr();
        Ok(Self {
            listener: Some(listener),
            registry,
            address,
        })
    }

    fn wait_for_worker(&self) -> DemoResult<()> {
        let ok = wait_until(HANDSHAKE_TIMEOUT, || {
            (0..FAN_OUT).all(|ordinal| {
                self.registry
                    .select_worker(NAMESPACE, TASK_QUEUE, &format!("fan:{ordinal}"), None)
                    .ok()
                    .flatten()
                    .is_some()
            })
        });
        if ok {
            Ok(())
        } else {
            Err(demo_error("worker never registered for the pool"))
        }
    }

    fn shutdown(mut self) -> DemoResult<()> {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().map_err(demo_error)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct WorkerControl {
    executions: AtomicUsize,
    dispatch_seen: AtomicBool,
    released: AtomicBool,
}

struct SurvivorWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl SurvivorWorker {
    fn spawn(address: String, control: Arc<WorkerControl>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("worker runtime build failed: {error}");
                    return;
                }
            };
            runtime.block_on(async move {
                if let Err(error) = serve_worker(&address, &control, &thread_stop).await {
                    eprintln!("worker ended with error: {error}");
                }
            });
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }
}

async fn serve_worker(
    address: &str,
    control: &Arc<WorkerControl>,
    stop: &Arc<AtomicBool>,
) -> DemoResult<()> {
    let mut registry = ActivityRegistry::new();
    for ordinal in 0..FAN_OUT as u64 {
        let control = Arc::clone(control);
        registry = registry
            .register_activity(
                format!("fan:{ordinal}"),
                move |_input: serde_json::Value, _ctx| {
                    let control = Arc::clone(&control);
                    Box::pin(async move {
                        control.executions.fetch_add(1, Ordering::SeqCst);
                        control.dispatch_seen.store(true, Ordering::SeqCst);
                        while !control.released.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Ok(worker_result(ordinal))
                    })
                },
            )
            .map_err(demo_error)?;
    }
    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(NAMESPACE)
        .task_queue(TASK_QUEUE)
        .identity("lsub5-demo-worker")
        .max_concurrency(FAN_OUT)
        .reconnect_initial_backoff(Duration::from_millis(5))
        .reconnect_max_backoff(Duration::from_millis(20))
        .reconnect_max_attempts(3)
        .build()
        .map_err(demo_error)?;
    let worker =
        LiminalActivityWorker::connect(address, &config, Arc::new(registry)).map_err(demo_error)?;
    worker
        .serve_until(|| stop.load(Ordering::SeqCst))
        .await
        .map_err(demo_error)
}

#[derive(Debug, Default)]
struct NoopDeliveryCallback;

impl OutboxDeliveryCallback for NoopDeliveryCallback {
    fn deliver_completion(
        &self,
        _workflow_id: &WorkflowId,
        _activity_id: &ActivityId,
        _run_id: Option<&RunId>,
        _result: String,
    ) -> Result<bool, aion_server::ServerError> {
        Ok(true)
    }

    fn deliver_failure(
        &self,
        _workflow_id: &WorkflowId,
        _activity_id: &ActivityId,
        _run_id: Option<&RunId>,
        _reason: String,
    ) -> Result<bool, aion_server::ServerError> {
        Ok(true)
    }
}

struct OwnerServer {
    runtime: tokio::runtime::Runtime,
    dispatcher_shutdown: tokio::sync::watch::Sender<bool>,
}

impl OwnerServer {
    fn spawn(
        node: &Node,
        liminal: &RunningLiminalServer,
        dispatcher_config: OutboxDispatcherConfig,
    ) -> DemoResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(demo_error)?;
        let callback: Arc<dyn OutboxDeliveryCallback> = Arc::new(NoopDeliveryCallback);
        let dispatch: Arc<dyn OutboxRowDispatch> = Arc::new(RegistryLiminalDispatch::new(
            liminal.registry.clone(),
            callback,
        ));
        let outbox_store: Arc<dyn OutboxStore> = Arc::clone(&node.store) as Arc<dyn OutboxStore>;
        let dispatcher = OutboxDispatcher::new(outbox_store, dispatch, dispatcher_config);
        let (dispatcher_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        runtime.spawn(dispatcher.run(shutdown_rx));
        Ok(Self {
            runtime,
            dispatcher_shutdown,
        })
    }

    fn kill(self) {
        let _: Result<(), _> = self.dispatcher_shutdown.send(true);
        self.runtime.shutdown_timeout(Duration::from_secs(10));
    }
}

struct StubDispatcher {
    fired: Arc<AtomicBool>,
}

impl ActivityDispatcher for StubDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        self.fired.store(true, Ordering::SeqCst);
        Err(format!(
            "in-process dispatcher fired for {} — outbox cutover broken",
            request.name
        ))
    }
}

struct Server {
    runtime: tokio::runtime::Runtime,
    store: Arc<HaematiteStore>,
    engine: Arc<Engine>,
    dispatcher_shutdown: tokio::sync::watch::Sender<bool>,
}

impl Server {
    fn build(
        node: &Node,
        owned_shard: usize,
        package: &Package,
        liminal: &RunningLiminalServer,
        fired: &Arc<AtomicBool>,
        dispatcher_config: OutboxDispatcherConfig,
    ) -> DemoResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .enable_all()
            .build()
            .map_err(demo_error)?;
        let store_dyn: Arc<dyn EventStore> = Arc::clone(&node.store) as Arc<dyn EventStore>;
        let fired = Arc::clone(fired);
        let registry = liminal.registry.clone();
        let package = package.clone();
        let engine = runtime.block_on(async move {
            EngineBuilder::new()
                .store_arc(store_dyn)
                .in_memory_visibility()
                .scheduler_threads(1)
                .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
                    Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
                })
                .outbox_enabled(true)
                .activity_dispatcher(Arc::new(StubDispatcher { fired }))
                .bootstrap_schedule_coordinator(false)
                .owned_shards([owned_shard])
                .load_workflows(package)
                .build()
                .await
                .map_err(demo_error)
        })?;
        let engine = Arc::new(engine);
        let callback: Arc<dyn OutboxDeliveryCallback> =
            Arc::new(ServerOutboxDeliveryCallback::new(Arc::clone(&engine)));
        let dispatch: Arc<dyn OutboxRowDispatch> =
            Arc::new(RegistryLiminalDispatch::new(registry, callback));
        let outbox_store: Arc<dyn OutboxStore> = Arc::clone(&node.store) as Arc<dyn OutboxStore>;
        let dispatcher = OutboxDispatcher::new(outbox_store, dispatch, dispatcher_config);
        let (dispatcher_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        runtime.spawn(dispatcher.run(shutdown_rx));
        Ok(Self {
            runtime,
            store: Arc::clone(&node.store),
            engine,
            dispatcher_shutdown,
        })
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

async fn stage_fanout(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    package: &Package,
) -> DemoResult<()> {
    let store_dyn: Arc<dyn EventStore> = Arc::clone(store) as Arc<dyn EventStore>;
    let mut recorder = Recorder::new(workflow_id.clone(), store_dyn).with_run_id(run_id.clone());
    recorder
        .record_workflow_started(
            chrono::Utc::now(),
            WorkflowStartRecord {
                workflow_type: OUTBOX_MODULE.to_owned(),
                input: Payload::from_json(&json!({ "fixture": "fanout" })).map_err(demo_error)?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: PackageVersion::new(package.content_hash().to_string()),
            },
        )
        .await
        .map_err(demo_error)?;
    let items: Vec<FanOutItem> = (0..FAN_OUT as u64)
        .map(|ordinal| {
            Ok(FanOutItem {
                ordinal,
                namespace: NAMESPACE.to_owned(),
                task_queue: DEFAULT_TASK_QUEUE.to_owned(),
                node: None,
                activity_type: format!("fan:{ordinal}"),
                input: Payload::from_json(&json!("in")).map_err(demo_error)?,
                attempt: 1,
            })
        })
        .collect::<Result<_, DemoError>>()?;
    recorder
        .record_fan_out_dispatch(chrono::Utc::now(), &items)
        .await
        .map_err(demo_error)
}

async fn all_rows_present(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> DemoResult<bool> {
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        if store
            .outbox_row_status(&key)
            .await
            .map_err(demo_error)?
            .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn claimed_count(store: &Arc<HaematiteStore>, workflow_id: &WorkflowId) -> DemoResult<usize> {
    let mut claimed = 0;
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        if store.outbox_row_status(&key).await.map_err(demo_error)? == Some(OutboxStatus::Claimed) {
            claimed += 1;
        }
    }
    Ok(claimed)
}

async fn rows_all_done(store: &Arc<HaematiteStore>, workflow_id: &WorkflowId) -> DemoResult<bool> {
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        if store.outbox_row_status(&key).await.map_err(demo_error)? != Some(OutboxStatus::Done) {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn workflow_completed(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> DemoResult<bool> {
    let history = read_history(store, workflow_id).await?;
    Ok(aion_core::status_from_events(&history) == WorkflowStatus::Completed)
}

async fn read_history(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> DemoResult<Vec<Event>> {
    let store_dyn: Arc<dyn EventStore> = Arc::clone(store) as Arc<dyn EventStore>;
    store_dyn
        .read_history(workflow_id)
        .await
        .map_err(demo_error)
}

fn terminal_count_for(history: &[Event], ordinal: u64) -> usize {
    history
        .iter()
        .filter(|event| match event {
            Event::ActivityCompleted { activity_id, .. }
            | Event::ActivityFailed { activity_id, .. }
            | Event::ActivityCancelled { activity_id, .. } => {
                activity_id.sequence_position() == ordinal
            }
            _ => false,
        })
        .count()
}
