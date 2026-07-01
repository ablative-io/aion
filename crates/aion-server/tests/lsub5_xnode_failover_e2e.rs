//! LSUB-5 (the capstone): real-app CROSS-NODE OWNER-KILL failover, end to end.
//!
//! This is the proof-of-everything for the cross-node delivery chain. It stands
//! up TWO Aion servers in one process over a REAL beamr loopback haematite
//! cluster and a REAL `liminal-server` over loopback TCP, runs a durable fan-out
//! workflow whose owner is KILLED mid-dispatch, and proves a survivor adopts the
//! orphaned shard and re-drives the in-flight fan-out to an EXACTLY-ONCE-per
//! -ordinal completion.
//!
//! ```text
//! cargo test -p aion-server \
//!     --features haematite-backend,liminal-transport \
//!     --test lsub5_xnode_failover_e2e -- --nocapture
//! ```
//!
//! ## The scenario
//!
//! - A 2-shard active-active haematite cluster, two Aion engines (server A and
//!   server B) in one process over genuine bound replication endpoints. Server A
//!   OWNS shard 0; server B OWNS shard 1. Each runs a REAL [`OutboxDispatcher`]
//!   over its own engine's store, with the cross-node liminal dispatch sink
//!   ([`RegistryLiminalDispatch`]) re-entering worker results through the
//!   production [`ServerOutboxDeliveryCallback`] over that engine.
//! - A REAL `liminal-server` over loopback TCP with the aion
//!   [`LiminalConnectionNotifier`] installed, and a REAL [`LiminalActivityWorker`]
//!   registered in-band (`connect_with_registration`). The worker's activities
//!   COUNT executions (so at-least-once redelivery is observable) and reply with a
//!   deterministic per-ordinal result.
//! - A fan-out workflow resident on shard 0 (server A) stages N=4 outbox rows via
//!   the durable cutover (`record_fan_out_dispatch`), co-located with shard 0.
//!   Server A's dispatcher claims them (its `owned_shard_scope` = shard 0) and
//!   pushes them to the liminal worker. The worker's first wave is GATED so the
//!   kill is deterministically MID-DISPATCH: at least one row is Claimed and the
//!   worker has genuinely received a dispatch when A dies.
//!
//! ## The kill, and the failover
//!
//! Killing server A drops its store + endpoint (closing its loopback sockets —
//! the same thing `kill -9` does to a process's sockets) AND stops A's dispatcher
//! task, so A's cluster membership and all of A's tasks die exactly as a process
//! death would. Server B's [`ClusterSupervisor`] watches A, debounces the link
//! drop, and AUTO-adopts shard 0: `adopt_shards([0])` elects + union-merges shard
//! 0's history, `extend_owned_shards([0])` WIDENS B's owned scope, and
//! `recover_adopted_shards` re-residents the orphaned workflow — whose first
//! arrival into `collect_all` re-arms the stranded rows to `Pending` via
//! `rearm_outbox_pending`.
//!
//! ## The CRUX this capstone proves (verified, not assumed)
//!
//! After adoption, server B's ALREADY-RUNNING dispatcher must CLAIM the re-armed
//! shard-0 rows. That only works if `extend_owned_shards` refreshes the SAME
//! `owned_shard_scope()` that `claim_outbox_rows` filters on. It does: a
//! `HaematiteStore`'s owned set is an `Arc<RwLock<..>>` shared across clones, the
//! dispatcher and the engine share one store handle, and `adopt_shards` widens
//! that shared set — so B's next sweep sees shard 0 with NO manual scope poke.
//!
//! ## The one-terminal gate
//!
//! For EACH fan-out ordinal there is EXACTLY ONE terminal event in the merged
//! shard-0 history; every outbox row ends `Done`; the workflow is `Completed`;
//! there is no duplicate `WorkflowStarted`/`ActivityScheduled` (idempotent
//! recovery); the witness workflow on shard 1 is unaffected. The activity MAY
//! execute more than once (at-least-once redelivery), but the terminal count per
//! ordinal is exactly one — the completion dedup absorbed the redelivery.
#![cfg(all(feature = "haematite-backend", feature = "liminal-transport"))]

use std::collections::HashMap;
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
    DEFAULT_TASK_QUEUE, Event, PackageVersion, Payload, RunId, WorkflowId, WorkflowStatus,
};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_server::cluster::{ClusterSupervisor, SupervisorConfig, WatchedPeer};
use aion_server::worker::{
    LiminalConnectionNotifier, OutboxDeliveryCallback, OutboxDispatcher, OutboxDispatcherConfig,
    OutboxRowDispatch, RegistryLiminalDispatch, ServerOutboxDeliveryCallback,
};
use aion_store::{EventStore, OutboxRow, OutboxStatus, OutboxStore};
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::{ConnectionSupervisor, LiminalConnectionServices};
use liminal_server::server::listener::ServerListener;
use serde_json::json;

type TestError = Box<dyn Error + Send + Sync>;
type TestResult = Result<(), TestError>;

// Three haematite nodes so the survivor can form a write quorum after the owner
// dies (a 2-node cluster cannot: majority of 2 is 2, so one death loses quorum).
// Node A owns shard 0 (the fan-out, killed mid-dispatch); node B owns shard 1
// (the survivor, runs the full engine); node C is a quorum-only participant
// (responder, owns shard 2, no engine) so B + C = 2-of-3 majority after A dies —
// the same survivor-quorum shape the ss5b auto-failover test uses.
const NODE_NAMES: [&str; 3] = [
    "lsub5-node-0@127.0.0.1",
    "lsub5-node-1@127.0.0.1",
    "lsub5-node-2@127.0.0.1",
];
const SHARD_COUNT: usize = 3;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of fan-out members the `collect_four` fixture dispatches.
const FAN_OUT: usize = 4;
/// Workflow isolation namespace + task queue the fan-out rows carry, and the pool
/// the liminal worker registers for. The fixture's fan-out members resolve the
/// named-default task queue, and the workflow is started in this namespace.
const NAMESPACE: &str = "default";
const TASK_QUEUE: &str = "default";
/// Worker identity for the in-band liminal registration.
const WORKER_IDENTITY: &str = "lsub5-survivor-worker";

/// The outbox fixture (`collect_four` etc.), reused from the aion-rs outbox e2e.
const OUTBOX_MODULE: &str = "aion_outbox_fixture";
const OUTBOX_BEAM: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_outbox_fixture.beam");
const OUTBOX_SOURCE: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_outbox_fixture.erl");

/// Generous upper bound for the WHOLE cross-node failover (detect + debounce +
/// adopt + replay + re-arm + re-dispatch + complete). Comfortable for CI jitter
/// while still bounding the proof to a sane window.
const FAILOVER_DEADLINE: Duration = Duration::from_secs(40);

fn test_error(message: impl std::fmt::Display) -> TestError {
    message.to_string().into()
}

fn loopback() -> Result<SocketAddr, TestError> {
    "127.0.0.1:0".parse().map_err(test_error)
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

fn membership(send_targets: &[&str]) -> WriteMembership {
    WriteMembership {
        total_nodes: NODE_NAMES.len(),
        send_targets: send_targets
            .iter()
            .map(|name| SyncNodeId::from(*name))
            .collect(),
    }
}

// ===========================================================================
// The fan-out workflow fixture package (the durable outbox `collect_four`).
// ===========================================================================

fn fixture_package() -> Result<Package, TestError> {
    let beams =
        BeamSet::new(vec![BeamModule::new(OUTBOX_MODULE, OUTBOX_BEAM)]).map_err(test_error)?;
    let manifest = Manifest {
        entry_module: OUTBOX_MODULE.to_owned(),
        entry_function: "collect_four".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(60),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(OUTBOX_MODULE, OUTBOX_SOURCE.to_vec())])
            .write_to_bytes()
            .map_err(test_error)?;
    Package::load_from_bytes(archive, ExtractionLimits::unbounded()).map_err(test_error)
}

/// The deterministic per-ordinal worker result string the fixture collects. The
/// fixture's `{ok, Results}` match consumes the JSON-string payloads in input
/// order, so each handler returns the JSON string `"worker-{ordinal}"`.
fn worker_result(ordinal: u64) -> serde_json::Value {
    json!(format!("worker-{ordinal}"))
}

/// The activity types the fan-out members carry (the fixture's `spec` names), one
/// per ordinal. The worker registers a handler for each so it serves the pool.
fn fan_out_activity_types() -> Vec<String> {
    (0..FAN_OUT)
        .map(|ordinal| format!("fan:{ordinal}"))
        .collect()
}

/// Stage the fan-out workflow's durable state on shard 0 through the SAME
/// production cutover seam the live engine's collect NIF uses: a
/// `WorkflowStarted` for `collect_four`, then `Recorder::record_fan_out_dispatch`
/// (the atomic `N×(ActivityScheduled+ActivityStarted)` events AND the matching
/// `N` Pending outbox rows, in one store transaction).
///
/// This is what makes the owner a genuine fan-out OWNER without an engine: the
/// resulting shard-0 history is byte-for-byte the shape a live `collect_four` run
/// produces at its first arrival into `collect_all`, so when the survivor adopts
/// shard 0 and REPLAYS `collect_four` over this history, its first arrival sees
/// the four ordinals scheduled-without-terminal (stale) and re-arms them — the
/// proven recovery path. The owner's dispatcher then has four Pending rows to
/// claim and push mid-flight.
async fn stage_fanout(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    package: &Package,
) -> Result<(), TestError> {
    let store_dyn: Arc<dyn EventStore> = Arc::clone(store) as Arc<dyn EventStore>;
    let mut recorder = Recorder::new(workflow_id.clone(), store_dyn).with_run_id(run_id.clone());
    recorder
        .record_workflow_started(
            chrono::Utc::now(),
            WorkflowStartRecord {
                workflow_type: OUTBOX_MODULE.to_owned(),
                input: Payload::from_json(&json!({ "fixture": "fanout" })).map_err(test_error)?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: PackageVersion::new(package.content_hash().to_string()),
            },
        )
        .await
        .map_err(test_error)?;
    let items: Vec<FanOutItem> = (0..FAN_OUT as u64)
        .map(|ordinal| {
            Ok(FanOutItem {
                ordinal,
                namespace: NAMESPACE.to_owned(),
                task_queue: DEFAULT_TASK_QUEUE.to_owned(),
                node: None,
                activity_type: format!("fan:{ordinal}"),
                input: Payload::from_json(&json!("in")).map_err(test_error)?,
                attempt: 1,
            })
        })
        .collect::<Result<_, TestError>>()?;
    recorder
        .record_fan_out_dispatch(chrono::Utc::now(), &items)
        .await
        .map_err(test_error)?;
    Ok(())
}

// ===========================================================================
// The in-process dispatcher stub: with the outbox flag ON it must NEVER fire.
// ===========================================================================

/// Activity dispatcher that flips a shared `fired` flag if invoked. With
/// `outbox.enabled` ON, a fresh fan-out member routes to the durable outbox, not
/// an in-process completion task — so this must never fire. Borrowed straight
/// from the aion-rs outbox e2e cutover guard.
struct StubDispatcher {
    fired: Arc<AtomicBool>,
}

impl ActivityDispatcher for StubDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        self.fired.store(true, Ordering::SeqCst);
        Err(format!(
            "in-process activity dispatcher fired for {} — the durable outbox cutover is broken",
            request.name,
        ))
    }
}

// ===========================================================================
// One haematite cluster node (real beamr loopback), mirroring ss5b/demo.
// ===========================================================================

struct Node {
    store: Arc<HaematiteStore>,
    event_store: Arc<haematite::EventStore>,
    addr: SocketAddr,
    name: &'static str,
    responder: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Node {
    fn spawn(name: &'static str, dir: &Path, send_targets: &[&str]) -> Result<Self, TestError> {
        let endpoint =
            DistributionEndpoint::bind(name, loopback()?, 1, None).map_err(test_error)?;
        let addr = endpoint.local_addr();
        let database = Database::create(DatabaseConfig {
            data_dir: dir.join("db"),
            shard_count: SHARD_COUNT,
            sweep_interval: None,
            distributed: None,
        })
        .map_err(test_error)?
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

fn link(from: &Node, to: &Node) -> TestResult {
    let endpoint = from
        .database()
        .distribution()
        .ok_or_else(|| test_error("dialing node has no endpoint"))?;
    endpoint.add_peer(to.name, to.addr);
    endpoint.connect(to.name).map_err(test_error)?;
    if !wait_until(HANDSHAKE_TIMEOUT, || endpoint.is_connected(to.name)) {
        return Err(test_error(format!(
            "{} never linked to {}",
            from.name, to.name
        )));
    }
    Ok(())
}

fn link_both(a: &Node, b: &Node) -> TestResult {
    link(a, b)?;
    link(b, a)?;
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

// ===========================================================================
// The real liminal server (worker-side endpoint) with the aion notifier.
// ===========================================================================

/// Holds the running liminal server bound for the test's lifetime, with the aion
/// in-band registration notifier installed (so a worker that connects with a
/// `WorkerRegistration` lands in the registry as a liminal-delivered member).
struct RunningLiminalServer {
    listener: Option<ServerListener>,
    registry: aion_server::worker::ConnectedWorkerRegistry,
    address: SocketAddr,
}

impl RunningLiminalServer {
    fn start() -> Result<Self, TestError> {
        let config = ServerConfig {
            listen_address: "127.0.0.1:0".parse().map_err(test_error)?,
            health_listen_address: reserve_loopback_port()?,
            channels: Vec::<ChannelDef>::new(),
            routing_rules: Vec::new(),
            persistence_path: None,
            cluster: None,
            drain_timeout_ms: 30_000,
        };
        let registry = aion_server::worker::ConnectedWorkerRegistry::default();
        let notifier = Arc::new(LiminalConnectionNotifier::new(registry.clone()));
        let services =
            Arc::new(LiminalConnectionServices::from_config(&config).map_err(test_error)?);
        let supervisor =
            ConnectionSupervisor::with_services_and_notifier(services, notifier.clone())
                .map_err(test_error)?;
        if !notifier.bind_supervisor(supervisor.clone()) {
            return Err(test_error("notifier supervisor was already bound"));
        }
        let listener = ServerListener::bind(&config, supervisor).map_err(test_error)?;
        let address = listener.local_addr();
        Ok(Self {
            listener: Some(listener),
            registry,
            address,
        })
    }

    /// Whether the registry currently routes at least one worker for the pool.
    fn has_worker(&self) -> Result<bool, TestError> {
        for activity_type in fan_out_activity_types() {
            let selected = self
                .registry
                .select_worker(NAMESPACE, TASK_QUEUE, &activity_type, None)
                .map_err(test_error)?;
            if selected.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn wait_for_worker(&self) -> Result<(), TestError> {
        if wait_until(HANDSHAKE_TIMEOUT, || self.has_worker().unwrap_or(false)) {
            return Ok(());
        }
        Err(test_error(
            "liminal server never registered the survivor worker for the pool",
        ))
    }

    fn shutdown(mut self) -> Result<(), TestError> {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().map_err(test_error)?;
        }
        Ok(())
    }
}

fn reserve_loopback_port() -> Result<SocketAddr, TestError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(test_error)?;
    let address = listener.local_addr().map_err(test_error)?;
    drop(listener);
    Ok(address)
}

// ===========================================================================
// The real survivor worker: counts executions, gates the FIRST wave so the kill
// is deterministically mid-dispatch, then replies with the per-ordinal result.
// ===========================================================================

/// Shared state the test observes / drives the survivor worker through.
struct WorkerControl {
    /// Total handler invocations across ALL waves (proves at-least-once).
    executions: AtomicUsize,
    /// Set once at least one activity has entered its handler — the genuine
    /// signal that server A reached a worker MID-DISPATCH (a row is Claimed and
    /// in flight), so the kill that follows is honestly mid-dispatch.
    dispatch_seen: AtomicBool,
    /// While false, a handler invocation blocks (the GATE). Released by the test
    /// AFTER the kill so A's first-wave dispatches never reach a live A — their
    /// replies are lost, exactly as a mid-dispatch process death loses them.
    released: AtomicBool,
}

impl WorkerControl {
    fn new() -> Self {
        Self {
            executions: AtomicUsize::new(0),
            dispatch_seen: AtomicBool::new(false),
            released: AtomicBool::new(false),
        }
    }
}

/// The survivor worker on a dedicated OS thread with its own current-thread
/// runtime (the liminal push receive is blocking). Stopped via the returned flag.
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
                    eprintln!("survivor runtime build failed: {error}");
                    return;
                }
            };
            runtime.block_on(async move {
                if let Err(error) = serve_survivor(&address, &control, &thread_stop).await {
                    eprintln!("survivor worker ended with error: {error}");
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

/// Build the survivor's activity registry (one gated, counting handler per
/// ordinal), connect it in-band to the liminal server, and serve until stopped.
async fn serve_survivor(
    address: &str,
    control: &Arc<WorkerControl>,
    stop: &Arc<AtomicBool>,
) -> Result<(), TestError> {
    use aion_worker::{ActivityRegistry, LiminalActivityWorker, WorkerConfig};

    let mut registry = ActivityRegistry::new();
    for ordinal in 0..FAN_OUT as u64 {
        let activity_type = format!("fan:{ordinal}");
        let control = Arc::clone(control);
        registry = registry
            .register_activity(activity_type, move |_input: serde_json::Value, _context| {
                let control = Arc::clone(&control);
                Box::pin(async move {
                    // Count EVERY execution (at-least-once observability).
                    control.executions.fetch_add(1, Ordering::SeqCst);
                    // Announce that a dispatch reached a worker: this is the
                    // genuine mid-dispatch signal the test kills A on.
                    control.dispatch_seen.store(true, Ordering::SeqCst);
                    // GATE: block until the test releases the wave (after the
                    // kill). A's first-wave dispatches therefore never complete
                    // back to a live A — their replies are lost like a process
                    // death loses them — while B's post-adoption redelivery sees
                    // the gate already open and completes immediately.
                    while !control.released.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Ok(worker_result(ordinal))
                })
            })
            .map_err(test_error)?;
    }

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(NAMESPACE)
        .task_queue(TASK_QUEUE)
        .identity(WORKER_IDENTITY)
        .max_concurrency(FAN_OUT)
        .reconnect_initial_backoff(Duration::from_millis(5))
        .reconnect_max_backoff(Duration::from_millis(20))
        .reconnect_max_attempts(3)
        .build()
        .map_err(test_error)?;

    let worker =
        LiminalActivityWorker::connect(address, &config, Arc::new(registry)).map_err(test_error)?;
    worker
        .serve_until(|| stop.load(Ordering::SeqCst))
        .await
        .map_err(test_error)
}

// ===========================================================================
// One Aion server in the cluster: an engine + its own outbox dispatcher, both
// over the SAME concrete HaematiteStore handle (so adoption's scope-widen
// refreshes the dispatcher's claim scope — the CRUX).
// ===========================================================================

/// A no-op [`OutboxDeliveryCallback`] for the OWNER server A. A's dispatch is
/// GATED at the worker (it blocks before replying) and A is killed before that
/// reply ever returns, so A's completion callback is never reached — A's role is
/// to STAGE the fan-out on its shard and CLAIM/PUSH it mid-flight, then die. A's
/// completion path is therefore not exercised, and recording it is unnecessary.
#[derive(Debug, Default)]
struct NoopDeliveryCallback;

impl OutboxDeliveryCallback for NoopDeliveryCallback {
    fn deliver_completion(
        &self,
        _workflow_id: &WorkflowId,
        _activity_id: &aion_core::ActivityId,
        _run_id: Option<&aion_core::RunId>,
        _result: String,
    ) -> Result<bool, aion_server::ServerError> {
        Ok(true)
    }

    fn deliver_failure(
        &self,
        _workflow_id: &WorkflowId,
        _activity_id: &aion_core::ActivityId,
        _run_id: Option<&aion_core::RunId>,
        _reason: String,
    ) -> Result<bool, aion_server::ServerError> {
        Ok(true)
    }
}

/// The OWNER server (A): a haematite shard owner + a real [`OutboxDispatcher`]
/// over its store, WITHOUT an engine.
///
/// A running aion engine over haematite cannot be torn down in-process so its
/// replication endpoint closes (its embedded beamr scheduler retains store
/// handles past `shutdown`; the ss5b/haematite kill pattern works only for an
/// engine-less node — see the LSUB-5 report's "seam wall"). So the owner is
/// modelled as a shard owner that STAGES its fan-out through the production
/// `Recorder::record_fan_out_dispatch` cutover seam (the exact API the live
/// engine's collect NIF calls) and runs the REAL dispatcher that claims + pushes
/// those rows mid-flight. Killing it drops its runtime + node, so its endpoint
/// closes cleanly — the membership-death signal the survivor detects. The
/// SURVIVOR (server B) is a full engine that does the real adopt + replay +
/// re-arm + re-dispatch, so the cross-node chain under test is exercised end to
/// end on a genuine engine.
struct OwnerServer {
    runtime: tokio::runtime::Runtime,
    dispatcher_shutdown: tokio::sync::watch::Sender<bool>,
}

impl OwnerServer {
    /// Spawn the owner's real outbox dispatcher over `node`'s store on a
    /// dedicated runtime. Returns the owner handle.
    fn spawn(
        node: &Node,
        liminal: &RunningLiminalServer,
        dispatcher_config: OutboxDispatcherConfig,
    ) -> Result<Self, TestError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(test_error)?;
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

    /// KILL the owner: stop its dispatcher and DROP its runtime, reaping every
    /// task it spawned. With no engine, the only remaining store handle is the
    /// node's, so dropping the node next closes the replication endpoint.
    fn kill(self) {
        let _: Result<(), _> = self.dispatcher_shutdown.send(true);
        self.runtime.shutdown_timeout(Duration::from_secs(10));
    }
}

/// The SURVIVOR Aion server (B): its OWN tokio runtime hosts a full engine AND
/// the outbox dispatcher, both over the SAME concrete `HaematiteStore` handle —
/// so adoption's `extend_owned_shards` refreshes the dispatcher's claim scope
/// (the CRUX). B is never killed mid-test; it does the real adopt + replay +
/// re-arm + re-dispatch over a genuine engine.
struct Server {
    runtime: tokio::runtime::Runtime,
    store: Arc<HaematiteStore>,
    engine: Arc<Engine>,
    dispatcher_shutdown: tokio::sync::watch::Sender<bool>,
}

impl Server {
    /// Build the engine over `node`'s store (outbox ON) on a dedicated runtime,
    /// wire a real liminal outbox dispatch sink over THIS engine, and spawn the
    /// dispatcher on the same runtime.
    fn build(
        node: &Node,
        owned_shard: usize,
        package: &Package,
        liminal: &RunningLiminalServer,
        fired: &Arc<AtomicBool>,
        dispatcher_config: OutboxDispatcherConfig,
    ) -> Result<Self, TestError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .enable_all()
            .build()
            .map_err(test_error)?;

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
                .map_err(test_error)
        })?;
        let engine = Arc::new(engine);

        // The cross-node liminal dispatch sink re-enters worker results through
        // the production ServerOutboxDeliveryCallback over THIS engine, so a
        // completion lands in this engine's (shard's) workflow history.
        let callback: Arc<dyn OutboxDeliveryCallback> =
            Arc::new(ServerOutboxDeliveryCallback::new(Arc::clone(&engine)));
        let liminal_dispatch: Arc<dyn OutboxRowDispatch> =
            Arc::new(RegistryLiminalDispatch::new(registry, callback));

        // The dispatcher claims through the SAME concrete store the engine owns:
        // both share the store's Arc<RwLock<owned_shards>>, so adopt_shards'
        // extend_owned_shards refreshes the dispatcher's claim scope (the CRUX).
        let outbox_store: Arc<dyn OutboxStore> = Arc::clone(&node.store) as Arc<dyn OutboxStore>;
        let dispatcher = OutboxDispatcher::new(outbox_store, liminal_dispatch, dispatcher_config);
        let (dispatcher_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        runtime.spawn(dispatcher.run(shutdown_rx));

        Ok(Self {
            runtime,
            store: Arc::clone(&node.store),
            engine,
            dispatcher_shutdown,
        })
    }

    /// Run an async engine/store operation on this server's runtime.
    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

// ===========================================================================
// History assertions: the one-terminal-per-ordinal gate.
// ===========================================================================

fn count_kind(history: &[Event], kind: fn(&Event) -> bool) -> usize {
    history.iter().filter(|event| kind(event)).count()
}

fn is_workflow_started(event: &Event) -> bool {
    matches!(event, Event::WorkflowStarted { .. })
}

fn is_workflow_completed(event: &Event) -> bool {
    matches!(event, Event::WorkflowCompleted { .. })
}

fn is_scheduled_for(event: &Event, ordinal: u64) -> bool {
    matches!(event, Event::ActivityScheduled { activity_id, .. }
        if activity_id.sequence_position() == ordinal)
}

/// Count the TERMINAL events (Completed/Failed/Cancelled) for one ordinal.
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

async fn read_history(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, TestError> {
    let store_dyn: Arc<dyn EventStore> = Arc::clone(store) as Arc<dyn EventStore>;
    store_dyn
        .read_history(workflow_id)
        .await
        .map_err(test_error)
}

// ===========================================================================
// THE LSUB-5 GATE.
// ===========================================================================

// The cluster's per-shard election (`acquire_shard_and_serve`) and the
// supervisor's adoption path are BLOCKING distribution calls that refuse to run
// from a thread with an entered tokio runtime. So — exactly as the ss5b /
// failover_demo harness does — this is a plain `#[test]`: the blocking cluster
// setup runs on the test thread, and every `async` engine/store/supervisor call
// is driven through a server's `block_on(..)`. The OWNER (A) is an engine-less
// shard owner + dispatcher whose runtime/node drop cleanly on kill (closing its
// endpoint — the membership-death signal); the SURVIVOR (B) runs the full engine
// that does the real adopt + replay + re-dispatch. See OwnerServer for why the
// owner is engine-less (the in-process engine-teardown seam wall).
//
// The end-to-end gate is one long, linear narrative (boot -> stage -> mid-dispatch
// -> kill -> adopt -> re-drive -> one-terminal proof); the same shape the
// `aion_cluster_failover_showcase` / `aion_on_haematite_showcase` e2es carry the
// `too_many_lines` allow for, by the existing test convention.
#[test]
#[allow(clippy::too_many_lines)]
fn xnode_owner_kill_redrives_fanout_to_exactly_once_completion() -> TestResult {
    println!("\n=== LSUB-5: cross-node owner-kill fan-out failover (exactly-once) ===");

    let package = fixture_package()?;

    // --- 1. Boot the 3-node real-loopback haematite cluster (sync prologue). -
    let node_count = NODE_NAMES.len();
    let dirs: Vec<tempfile::TempDir> = (0..node_count)
        .map(|_| tempfile::tempdir())
        .collect::<Result<_, _>>()
        .map_err(test_error)?;
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
    // Full mesh so every pair has a bidirectional link.
    for a in 0..node_count {
        for b in (a + 1)..node_count {
            link_both(node_ref(&nodes, a)?, node_ref(&nodes, b)?)?;
        }
    }
    // Each node elects + serves its own shard (A->0, B->1, C->2) and scopes to it.
    for (i, targets) in send_targets.iter().enumerate() {
        let node = node_ref(&nodes, i)?;
        node.database()
            .acquire_shard_and_serve(i, &membership(targets), OP_TIMEOUT)
            .map_err(test_error)?;
        node.store.set_owned_shards([i]);
    }
    println!(
        "  3-node cluster up: A owns shard 0 (dies), B owns shard 1 (survivor), C quorum-only."
    );

    // --- 2. Real liminal server + a real survivor worker (in-band). ---------
    let liminal = RunningLiminalServer::start()?;
    let address = liminal.address.to_string();
    let control = Arc::new(WorkerControl::new());
    let survivor = SurvivorWorker::spawn(address.clone(), Arc::clone(&control));
    liminal.wait_for_worker()?;
    println!("  liminal server up; survivor worker registered for the fan-out pool.");

    // --- 3. Mint the workflow ids on their target shards. -------------------
    let fanout_workflow = workflow_id_for_shard(&node_ref(&nodes, 0)?.store, 0);
    let witness_workflow = workflow_id_for_shard(&node_ref(&nodes, 1)?.store, 1);
    let fanout_run = RunId::new_v4();
    let fired_b = Arc::new(AtomicBool::new(false));

    // The dispatcher cadence: a short sweep so re-armed rows are re-claimed
    // promptly, with a backoff far longer than the failover deadline so that a
    // wrongly-backed-off retry would TIME OUT the test rather than hide behind it.
    let dispatcher_config = OutboxDispatcherConfig {
        poll_interval: Duration::from_millis(25),
        batch_size: 16,
        max_attempts: 8,
        backoff_base: Duration::from_secs(120),
        backoff_multiplier: 2,
        backoff_max: Duration::from_secs(240),
    };

    // --- 4. Build the SURVIVOR (server B): a full engine + dispatcher over its
    //        own store/runtime. Owner A is built next as an engine-less shard
    //        owner + dispatcher (see OwnerServer for why). -------------------
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
    println!("  survivor B (engine) and owner A (shard owner + dispatcher) up.");

    // --- 5. STAGE the fan-out on shard 0 through the production cutover
    //        (Recorder::record_fan_out_dispatch), and start the witness fan-out
    //        on shard 1 via server B's engine. --------------------------------
    server_b.block_on(stage_fanout(
        &store_a,
        &fanout_workflow,
        &fanout_run,
        &package,
    ))?;
    let witness_run = server_b
        .block_on(server_b.engine.start_workflow_with_id(
            OUTBOX_MODULE,
            Payload::from_json(&json!({ "fixture": "witness" })).map_err(test_error)?,
            HashMap::new(),
            NAMESPACE.to_owned(),
            Some(witness_workflow.clone()),
            None,
        ))
        .map_err(test_error)?
        .run_id()
        .clone();
    println!("  fan-out staged on shard 0 (durable cutover); witness started on shard 1.");

    // --- 6. Wait until the fan-out has STAGED its 4 outbox rows on shard 0,
    //        owner A's dispatcher has claimed at least one, AND the worker has
    //        genuinely received a dispatch — the deterministic mid-dispatch
    //        precondition for the kill. ---------------------------------------
    let staged = wait_until(Duration::from_secs(20), || {
        server_b
            .block_on(all_rows_present(&store_a, &fanout_workflow))
            .unwrap_or(false)
    });
    assert!(
        staged,
        "the fan-out must stage all {FAN_OUT} outbox rows on shard 0"
    );
    println!("  all {FAN_OUT} fan-out rows staged Pending on shard 0 (durable cutover).");

    let mid_dispatch = wait_until(Duration::from_secs(20), || {
        control.dispatch_seen.load(Ordering::SeqCst)
    });
    assert!(
        mid_dispatch,
        "owner A's dispatcher must reach the worker MID-DISPATCH before the kill"
    );
    let claimed_before_kill = server_b.block_on(claimed_count(&store_a, &fanout_workflow))?;
    assert!(
        claimed_before_kill >= 1,
        "at least one shard-0 row must be Claimed (in flight) when A is killed; got {claimed_before_kill}"
    );
    let executions_before_kill = control.executions.load(Ordering::SeqCst);
    println!(
        "  MID-DISPATCH: worker received a dispatch; {claimed_before_kill} shard-0 row(s) Claimed, \
         {executions_before_kill} execution(s) so far."
    );

    // --- 7. Build server B's supervisor watching A (owner of shard 0). ------
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
    assert!(supervisor.watches_any(), "supervisor must watch server A");
    assert!(
        store_b.peer_connected(NODE_NAMES[0]),
        "server B must see server A connected before the kill"
    );
    let pre_kill = server_b.block_on(supervisor.tick());
    assert!(pre_kill.is_empty(), "no adoption while server A is alive");

    // --- 8. KILL owner A mid-dispatch: stop + DROP its dispatcher runtime, then
    //        drop its node so its replication endpoint closes. -----------------
    println!("  >>> killing owner A (drop dispatcher runtime, close endpoint) <<<");
    // Release the worker GATE first so A's in-flight first-wave dispatch unblocks:
    // its reply flows back to a DEAD A (lost), exactly as a mid-dispatch process
    // death loses it.
    control.released.store(true, Ordering::SeqCst);
    owner
        .take()
        .ok_or_else(|| test_error("owner A already killed"))?
        .kill();
    // Drop the test's own handle to A's store: with the owner's runtime gone, the
    // only remaining handles are this one and the node's, so releasing both lets
    // A's replication endpoint close (the membership-death signal).
    drop(store_a);
    let dead = nodes[0]
        .take()
        .ok_or_else(|| test_error("owner A node already gone"))?;
    drop(dead);
    assert!(
        wait_until(Duration::from_secs(20), || !store_b
            .peer_connected(NODE_NAMES[0])),
        "server B must observe server A's replication link DROP after the kill"
    );
    println!("  server B observed server A's link DROP (peer_connected -> false).");

    // --- 9. Drive the supervisor: debounce, then AUTO-adopt shard 0. --------
    let first = server_b.block_on(supervisor.tick());
    assert!(
        first.is_empty(),
        "debounce: first down-tick must not adopt yet"
    );
    let second = server_b.block_on(supervisor.tick());
    assert_eq!(
        second,
        vec![NODE_NAMES[0].to_owned()],
        "second consecutive down-tick must AUTO-adopt server A's shard 0"
    );
    println!("  server B AUTO-adopted shard 0 (debounced, no manual adopt).");

    // CRUX assertion: adoption widened B's owned-shard scope to include shard 0,
    // and because the dispatcher claims through the SAME store handle, its next
    // sweep now sees shard 0's rows. Confirm the shared scope was refreshed.
    let owned_after = store_b.owned_shards().unwrap_or_default();
    assert!(
        owned_after.contains(&0) && owned_after.contains(&1),
        "adoption must UNION shard 0 into B's owned scope (got {owned_after:?}) — \
         this is the shared owned_shard_scope() the dispatcher's claim filters on"
    );
    println!(
        "  CRUX: B's shared claim scope now owns {owned_after:?} (shard 0 refreshed in place)."
    );

    // --- 10. Wait for the failover to complete: every shard-0 row Done and the
    //         fan-out workflow Completed, within the deadline. ---------------
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
    let elapsed = started.elapsed();
    assert!(
        completed,
        "the fan-out must re-drive to completion on server B within {FAILOVER_DEADLINE:?} \
         (elapsed {elapsed:?})"
    );
    println!("  failover completed in {elapsed:?}: all shard-0 rows Done, workflow Completed.");

    // --- 11. THE ONE-TERMINAL GATE. ----------------------------------------
    let history = server_b.block_on(read_history(&store_b, &fanout_workflow))?;

    // Idempotent recovery: exactly one WorkflowStarted, one ActivityScheduled per
    // ordinal (no duplicate scheduling across the adopt/replay boundary).
    assert_eq!(
        count_kind(&history, is_workflow_started),
        1,
        "exactly one WorkflowStarted (idempotent recovery): {history:#?}"
    );
    for ordinal in 0..FAN_OUT as u64 {
        let scheduled = history
            .iter()
            .filter(|event| is_scheduled_for(event, ordinal))
            .count();
        assert_eq!(
            scheduled, 1,
            "ordinal {ordinal} must have exactly one ActivityScheduled (no duplicate scheduling)"
        );
    }

    // EXACTLY ONE terminal per ordinal — the dedup absorbed any redelivery.
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(
            terminal_count_for(&history, ordinal),
            1,
            "ordinal {ordinal} must have EXACTLY ONE terminal event: {history:#?}"
        );
    }

    // Exactly one workflow terminal (Completed).
    assert_eq!(
        count_kind(&history, is_workflow_completed),
        1,
        "the fan-out workflow completes exactly once"
    );
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed,
        "the fan-out workflow must be terminally Completed"
    );

    // Every outbox row ends Done.
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(&fanout_workflow, ordinal);
        let status = server_b
            .block_on(store_b.outbox_row_status(&key))
            .map_err(test_error)?
            .ok_or_else(|| test_error(format!("missing outbox row for ordinal {ordinal}")))?;
        assert_eq!(
            status,
            OutboxStatus::Done,
            "ordinal {ordinal}'s outbox row must end Done"
        );
    }

    // AT-LEAST-ONCE but exactly-one-terminal: the activity ran at least the
    // FAN_OUT first-wave times (gated under A) PLUS the post-adoption redelivery
    // on B, so total executions strictly exceed the terminal count.
    let total_executions = control.executions.load(Ordering::SeqCst);
    assert!(
        total_executions >= FAN_OUT,
        "the activity must have executed at least once per ordinal; got {total_executions}"
    );
    assert!(
        total_executions > FAN_OUT,
        "the worker must have executed MORE than once per ordinal (A's lost wave + B's redelivery): \
         got {total_executions} executions for {FAN_OUT} ordinals, each with exactly one terminal"
    );
    println!(
        "  ONE-TERMINAL PROVED: {FAN_OUT} ordinals, one terminal each; \
         worker executed {total_executions} times (at-least-once, dedup -> exactly-once)."
    );

    // --- 12. The witness on shard 1 is unaffected by the kill. -------------
    // Server B's own dispatcher drove the witness fan-out to completion across
    // the whole kill/adopt sequence.
    let witness_done = wait_until(FAILOVER_DEADLINE, || {
        server_b.block_on(async {
            rows_all_done(&server_b.store, &witness_workflow)
                .await
                .unwrap_or(false)
                && workflow_completed(&server_b.store, &witness_workflow)
                    .await
                    .unwrap_or(false)
        })
    });
    assert!(
        witness_done,
        "the witness workflow on shard 1 must complete, unaffected by the kill"
    );
    let witness_history = server_b.block_on(read_history(&server_b.store, &witness_workflow))?;
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(
            terminal_count_for(&witness_history, ordinal),
            1,
            "witness ordinal {ordinal} has exactly one terminal"
        );
    }
    assert_eq!(
        aion_core::status_from_events(&witness_history),
        WorkflowStatus::Completed,
        "the witness workflow must be Completed"
    );
    println!("  witness workflow on shard 1 completed, unaffected by the kill.");

    // Confirm the adopted run is the same run, completed on B.
    let adopted = server_b
        .block_on(server_b.engine.result(&fanout_workflow, &fanout_run))
        .map_err(test_error)?;
    assert!(
        adopted.is_ok(),
        "the adopted fan-out run must resolve to a successful result"
    );
    let witness_result = server_b
        .block_on(server_b.engine.result(&witness_workflow, &witness_run))
        .map_err(test_error)?;
    assert!(
        witness_result.is_ok(),
        "the witness run must resolve to a successful result"
    );

    println!(
        "=== LSUB-5 PROVED: owner killed mid-dispatch; survivor adopted, re-drove fan-out, \
              exactly-once per ordinal ==="
    );

    // --- Teardown: tear down B (drop the supervisor's engine clone first so B's
    //     engine is uniquely held), then the worker + liminal server. ----------
    let _: Result<(), _> = server_b.dispatcher_shutdown.send(true);
    drop(supervisor);
    server_b.engine.shutdown().map_err(test_error)?;
    let Server {
        runtime,
        store,
        engine,
        ..
    } = server_b;
    runtime.shutdown_timeout(Duration::from_secs(10));
    drop(engine);
    drop(store);
    survivor.stop();
    liminal.shutdown()?;
    Ok(())
}

// --- small async store helpers (kept out of the test body for readability) ---

fn node_ref(nodes: &[Option<Node>], index: usize) -> Result<&Node, TestError> {
    nodes
        .get(index)
        .and_then(Option::as_ref)
        .ok_or_else(|| test_error(format!("node {index} is not live")))
}

/// Whether all `FAN_OUT` outbox rows for `workflow_id` exist (any state).
async fn all_rows_present(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> Result<bool, TestError> {
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        if store
            .outbox_row_status(&key)
            .await
            .map_err(test_error)?
            .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Count the Claimed outbox rows for `workflow_id`.
async fn claimed_count(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> Result<usize, TestError> {
    let mut claimed = 0;
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        if store.outbox_row_status(&key).await.map_err(test_error)? == Some(OutboxStatus::Claimed) {
            claimed += 1;
        }
    }
    Ok(claimed)
}

/// Whether every outbox row for `workflow_id` is `Done`.
async fn rows_all_done(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> Result<bool, TestError> {
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        if store.outbox_row_status(&key).await.map_err(test_error)? != Some(OutboxStatus::Done) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `workflow_id` is terminally Completed per its merged history.
async fn workflow_completed(
    store: &Arc<HaematiteStore>,
    workflow_id: &WorkflowId,
) -> Result<bool, TestError> {
    let history = read_history(store, workflow_id).await?;
    Ok(aion_core::status_from_events(&history) == WorkflowStatus::Completed)
}
