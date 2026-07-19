//! LSUB-L2: in-band cross-node work dispatch over liminal (end-to-end).
//!
//! The whole file is gated on `liminal-transport`, so a default build never
//! compiles it and never links liminal. It stands up a REAL `liminal-server` over
//! loopback TCP with the aion `LiminalConnectionNotifier` installed, and a REAL
//! remote `aion-worker` (`LiminalActivityWorker`) that connects WITH an in-band
//! `WorkerRegistration` (`connect_with_registration`). The server's notifier
//! auto-registers the worker into the EXISTING connected-worker registry as a
//! liminal-delivered member, a pushed `DispatchRequest` is executed through the
//! worker's real activity registry, and the correlated `DispatchResponse`
//! re-enters aion through the SAME `OutboxDeliveryCallback` the gRPC completion
//! path uses. On worker disconnect the notifier deregisters it.
//!
//! This RETIRES the LSUB-1 out-of-band registration hack: there is no
//! `active_connection_pids()` loop and no hard-coded `register_liminal_worker`
//! helper — the worker self-describes and the server reacts in-band.
//!
//! The proof:
//!
//! - `xnode_inband_dispatch_routes_executes_and_completes` — the worker connects
//!   with a registration, the installed notifier registers it, a staged outbox
//!   row for the worker's `(namespace, task_queue, node)` pool is claimed
//!   (scoped), pushed to the worker, executed, and its terminal completion is
//!   recorded exactly once through the delivery callback; the worker observably
//!   ran the activity. On worker disconnect the server deregisters it (the
//!   registry no longer routes to its pool).
//! - `dispatch_for_a_different_pool_is_not_delivered` — a row for a DIFFERENT pool
//!   selects no worker, so the dispatch returns an honest no-worker error (the
//!   outbox would retry) and the worker never runs it; routing selection is the
//!   same registry semantics the gRPC path uses.
#![cfg(feature = "liminal-transport")]

use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aion_core::{ActivityId, RunId, WorkflowId};
use aion_server::ServerError;
use aion_server::worker::{
    ConnectedWorkerRegistry, LiminalConnectionNotifier, OutboxDeliveryCallback, OutboxRowDispatch,
    RegistryLiminalDispatch,
};
use aion_store::{ClaimScope, OutboxRow, OutboxStatus, OutboxStore};
use aion_store_libsql::LibSqlStore;
use aion_worker::{ActivityRegistry, LiminalActivityWorker, WorkerConfig};
use chrono::Utc;
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::ConnectionSupervisor;
use liminal_server::server::listener::ServerListener;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use uuid::Uuid;

type TestError = Box<dyn Error + Send + Sync>;

/// One recorded completion: the correlation ids plus the worker's result string.
type CompletionRecord = (WorkflowId, ActivityId, Option<RunId>, String);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const NAMESPACE: &str = "remote";
const TASK_QUEUE: &str = "gpu";
const NODE: &str = "box-7";
const ACTIVITY_TYPE: &str = "charge-card";

/// The activity input/output the test handler round-trips, proving the worker
/// genuinely executed the pushed dispatch (not an echo).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChargeInput {
    amount: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChargeOutput {
    charged: bool,
    amount: u64,
}

fn test_error(message: impl std::fmt::Display) -> TestError {
    message.to_string().into()
}

/// Recording [`OutboxDeliveryCallback`] standing in for the prod
/// `ServerOutboxDeliveryCallback`. Records each completion so the test can assert
/// the worker result re-entered aion through the shared seam exactly once.
#[derive(Debug, Default)]
struct RecordingCallback {
    completions: Mutex<Vec<CompletionRecord>>,
}

impl OutboxDeliveryCallback for RecordingCallback {
    fn deliver_completion(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        result: String,
    ) -> Result<bool, ServerError> {
        if let Ok(mut completions) = self.completions.lock() {
            completions.push((
                workflow_id.clone(),
                activity_id.clone(),
                run_id.cloned(),
                result,
            ));
        }
        Ok(true)
    }

    fn deliver_failure(
        &self,
        _workflow_id: &WorkflowId,
        _activity_id: &ActivityId,
        _run_id: Option<&RunId>,
        _reason: String,
    ) -> Result<bool, ServerError> {
        Ok(true)
    }
}

/// Holds the running liminal server bound for the lifetime of a test, with the
/// aion in-band registration notifier installed.
struct RunningServer {
    listener: Option<ServerListener>,
    registry: ConnectedWorkerRegistry,
    address: SocketAddr,
}

impl RunningServer {
    fn start() -> Result<Self, TestError> {
        let config = ServerConfig {
            listen_address: "127.0.0.1:0".parse().map_err(test_error)?,
            health_listen_address: reserve_loopback_port()?,
            // The push transport does not require a configured channel; an empty
            // set is enough to stand up the listener + connection supervisor.
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

        // The aion-side connected-worker registry the notifier registers into and
        // the dispatch path selects from — the SAME registry, the SAME selection,
        // as a gRPC worker.
        let registry = ConnectedWorkerRegistry::default();

        // Resolve the notifier <-> supervisor construction cycle: build the
        // notifier (it does not yet hold a supervisor), construct the supervisor
        // WITH the notifier, then immediately bind the supervisor into the
        // notifier so its in-band registrations can build a push delivery.
        let notifier = Arc::new(LiminalConnectionNotifier::new(registry.clone()));
        let supervisor = build_supervisor_with_notifier(&config, notifier.clone())?;
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

    /// Waits until the registry has a worker for the test pool (the notifier
    /// registered the connected worker in-band) and returns its handle's presence.
    fn wait_for_registered_worker(&self) -> Result<(), TestError> {
        self.wait_until(true, "server never registered the in-band worker")
    }

    /// Waits until the registry NO LONGER has a worker for the test pool (the
    /// notifier deregistered it on disconnect).
    fn wait_for_deregistered_worker(&self) -> Result<(), TestError> {
        self.wait_until(false, "server never deregistered the worker on disconnect")
    }

    fn wait_until(&self, present: bool, on_timeout: &str) -> Result<(), TestError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            let selected = self
                .registry
                .select_worker(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, Some(NODE))
                .map_err(test_error)?;
            if selected.is_some() == present {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(test_error(on_timeout))
    }

    fn shutdown(mut self) -> Result<(), TestError> {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().map_err(test_error)?;
        }
        Ok(())
    }
}

/// Builds a connection supervisor that carries the aion in-band registration
/// notifier, sourcing its connection services the same way
/// [`ConnectionSupervisor::from_config`] does for the test's channel-free config.
fn build_supervisor_with_notifier(
    config: &ServerConfig,
    notifier: Arc<LiminalConnectionNotifier>,
) -> Result<ConnectionSupervisor, TestError> {
    use liminal_server::server::connection::LiminalConnectionServices;

    let services = Arc::new(LiminalConnectionServices::from_config(config).map_err(test_error)?);
    ConnectionSupervisor::with_services_and_notifier(services, notifier).map_err(test_error)
}

fn reserve_loopback_port() -> Result<SocketAddr, TestError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(test_error)?;
    let address = listener.local_addr().map_err(test_error)?;
    drop(listener);
    Ok(address)
}

/// Builds the test activity registry, counting executions so the test can assert
/// the worker genuinely ran the activity (vs. a no-op or echo).
fn worker_registry(executions: Arc<AtomicUsize>) -> Result<Arc<ActivityRegistry>, TestError> {
    let registry = ActivityRegistry::new()
        .register_activity(ACTIVITY_TYPE, move |input: ChargeInput, _context| {
            let executions = Arc::clone(&executions);
            Box::pin(async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ChargeOutput {
                    charged: true,
                    amount: input.amount,
                })
            })
        })
        .map_err(test_error)?;
    Ok(Arc::new(registry))
}

/// The worker config the remote worker self-describes with: it registers into the
/// test pool `(NAMESPACE, TASK_QUEUE, NODE)` with the activity it serves.
fn worker_config() -> Result<WorkerConfig, TestError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(NAMESPACE)
        .task_queue(TASK_QUEUE)
        .node(NODE)
        .identity("lsub-l2-worker")
        .max_concurrency(1)
        .reconnect_initial_backoff(Duration::from_millis(5))
        .reconnect_max_backoff(Duration::from_millis(20))
        .reconnect_max_attempts(3)
        .build()
        .map_err(test_error)
}

/// Spawns the liminal worker on a dedicated OS thread with its own current-thread
/// runtime. The push client's receive is a blocking call, so the worker is driven
/// off the test's runtime; the returned flag stops it.
struct WorkerThread {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl WorkerThread {
    fn spawn(address: String, config: WorkerConfig, registry: Arc<ActivityRegistry>) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
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
                // SELF-REGISTER in-band: connect_with_registration runs the
                // WorkerRegister -> WorkerRegisterAck round-trip before serving.
                let worker = match LiminalActivityWorker::connect(&address, &config, registry) {
                    Ok(worker) => worker,
                    Err(error) => {
                        eprintln!("worker connect failed: {error}");
                        return;
                    }
                };
                if let Err(error) = worker
                    .serve_until(|| thread_stop.load(Ordering::SeqCst))
                    .await
                {
                    eprintln!("worker serve loop ended with error: {error}");
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
            handle.join().ok();
        }
    }
}

/// Builds a pending outbox row for the worker's `(namespace, task_queue, node)`
/// pool.
fn pending_row(
    namespace: &str,
    task_queue: &str,
    node: Option<&str>,
    ordinal: u64,
) -> Result<OutboxRow, TestError> {
    let workflow_id = WorkflowId::new(Uuid::new_v4());
    let dispatch_key = format!("{workflow_id}:{ordinal}");
    let input =
        aion_core::Payload::from_json(&serde_json::json!({ "amount": 42 })).map_err(test_error)?;
    Ok(OutboxRow {
        dispatch_key,
        workflow_id,
        ordinal,
        run_id: Some(RunId::new(Uuid::new_v4())),
        namespace: namespace.to_owned(),
        task_queue: task_queue.to_owned(),
        node: node.map(ToOwned::to_owned),
        activity_type: ACTIVITY_TYPE.to_owned(),
        input,
        status: OutboxStatus::Pending,
        attempt: 0,
        visible_after: Utc::now(),
        claimed_at: None,
    })
}

/// Opens a fresh on-disk libsql outbox store for the test.
async fn open_store(name: &str) -> Result<Arc<LibSqlStore>, TestError> {
    let nanos = Instant::now().elapsed().as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aion-lsub1-{name}-{}-{nanos}.db",
        std::process::id()
    ));
    LibSqlStore::open(path)
        .await
        .map(Arc::new)
        .map_err(test_error)
}

/// THE LOAD-BEARING TEST: a remote worker self-registers IN-BAND, the installed
/// notifier auto-registers it, a staged outbox row for the worker's pool is
/// claimed (scoped), pushed to the worker over liminal, executed, and its
/// terminal completion recorded exactly once through the shared callback. On
/// worker disconnect the notifier deregisters it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn xnode_inband_dispatch_routes_executes_and_completes() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address.to_string();

    // A REAL remote worker connects over the liminal push transport WITH an
    // in-band registration; the server's notifier auto-registers it.
    let executions = Arc::new(AtomicUsize::new(0));
    let registry_for_worker = worker_registry(Arc::clone(&executions))?;
    let worker = WorkerThread::spawn(address.clone(), worker_config()?, registry_for_worker);

    // The installed notifier inserted the worker into the EXISTING registry as a
    // liminal-delivered member for (remote, gpu, box-7) — entirely in-band.
    server.wait_for_registered_worker()?;
    let registry = server.registry.clone();

    // Stage one pending row for that pool in a real durable outbox, then claim it
    // SCOPED to the pool (LSUB-1a) — only the owned pool's rows are claimed.
    let store = open_store("dispatch").await?;
    let row = pending_row(NAMESPACE, TASK_QUEUE, Some(NODE), 0)?;
    store
        .append_outbox_batch(std::slice::from_ref(&row))
        .await
        .map_err(test_error)?;
    let scope = ClaimScope::new(NAMESPACE, TASK_QUEUE).with_node(Some(NODE.to_owned()));
    let claimed = store
        .claim_outbox_rows_scoped(&scope, 16)
        .await
        .map_err(test_error)?;
    let claimed_row = claimed
        .into_iter()
        .find(|candidate| candidate.dispatch_key == row.dispatch_key)
        .ok_or_else(|| test_error("scoped claim did not return the staged row"))?;

    // DISPATCH: select the worker by (ns, tq, type, node), push the request to its
    // connection, execute, and re-enter the result through the shared callback.
    let callback = Arc::new(RecordingCallback::default());
    let dispatch = RegistryLiminalDispatch::new(
        registry.clone(),
        Arc::clone(&callback) as Arc<dyn OutboxDeliveryCallback>,
    );
    dispatch
        .dispatch(&claimed_row)
        .await
        .map_err(|error| test_error(format!("cross-node dispatch returned Err: {error}")))?;

    // The worker genuinely executed the activity exactly once.
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the remote worker must have executed the dispatched activity exactly once"
    );

    // The worker's result re-entered aion through the shared callback, correlated
    // to the exact workflow / ordinal / run that was dispatched — the terminal.
    {
        let completions = callback
            .completions
            .lock()
            .map_err(|_| test_error("completions lock poisoned"))?;
        assert_eq!(completions.len(), 1, "exactly one terminal completion");
        let (workflow_id, activity_id, run_id, result) = completions
            .first()
            .ok_or_else(|| test_error("no completion recorded"))?;
        assert_eq!(workflow_id, &row.workflow_id);
        assert_eq!(
            activity_id,
            &ActivityId::from_sequence_position(row.ordinal)
        );
        assert_eq!(
            run_id, &row.run_id,
            "run_id survived the liminal round trip"
        );
        // The activity genuinely ran the handler (charged: true, amount echoed).
        let output: ChargeOutput = serde_json::from_str(result).map_err(test_error)?;
        assert!(output.charged, "the handler ran and charged");
        assert_eq!(output.amount, 42, "the handler saw the dispatched input");
    }

    // DEREGISTRATION: when the worker disconnects, the server's notifier fires
    // on_worker_unregistered and drops the registration guard, so the registry no
    // longer routes to the worker's pool.
    worker.stop();
    server.wait_for_deregistered_worker()?;
    assert!(
        registry
            .select_worker(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, Some(NODE))
            .map_err(test_error)?
            .is_none(),
        "the disconnected worker must be deregistered from the registry"
    );

    server.shutdown()?;
    Ok(())
}

/// ROUTING CHECK: a row for a DIFFERENT pool selects no worker, so the dispatch
/// returns an honest no-worker error (the outbox would retry) and the worker never
/// runs it. This reuses the registry's selection semantics: the worker registered
/// (in-band) only for (remote, gpu, box-7) is not a candidate for (other, cpu).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_for_a_different_pool_is_not_delivered() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address.to_string();

    let executions = Arc::new(AtomicUsize::new(0));
    let registry_for_worker = worker_registry(Arc::clone(&executions))?;
    let worker = WorkerThread::spawn(address.clone(), worker_config()?, registry_for_worker);
    server.wait_for_registered_worker()?;
    let registry = server.registry.clone();

    // A row for a DIFFERENT (namespace, task_queue) pool than the worker serves.
    let other_row = pending_row("other", "cpu", None, 0)?;

    let callback = Arc::new(RecordingCallback::default());
    let dispatch = RegistryLiminalDispatch::new(
        registry.clone(),
        Arc::clone(&callback) as Arc<dyn OutboxDeliveryCallback>,
    );
    let result = dispatch.dispatch(&other_row).await;
    assert!(
        result.is_err(),
        "a dispatch for a pool with no matching worker must return Err so the outbox retries"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "the worker must not execute a dispatch for a pool it does not serve"
    );
    assert_eq!(
        callback
            .completions
            .lock()
            .map_err(|_| test_error("completions lock poisoned"))?
            .len(),
        0,
        "no completion is recorded for an undelivered dispatch"
    );

    worker.stop();
    server.shutdown()?;
    Ok(())
}
