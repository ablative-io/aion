//! LSUB-3 Stage B: fast cross-node failover when a worker dies mid-dispatch.
//!
//! The whole file is gated on `liminal-transport`, so a default build never
//! compiles it and never links liminal. It stands up a REAL `liminal-server` over
//! loopback TCP with the aion `LiminalConnectionNotifier` installed and registers
//! TWO workers for the SAME `(namespace, task_queue, node)` pool:
//!
//! - **Worker A — the doomed worker.** A raw liminal `PushClient` that registers
//!   in-band (so the server's notifier inserts it into the connected-worker
//!   registry as a liminal-delivered member), receives exactly ONE pushed
//!   dispatch, signals that it received it, then DROPS its client — closing the
//!   socket WITHOUT sending a correlated reply. Because `select_worker` picks the
//!   LOWEST worker id, and Worker A registers first, the first dispatch is
//!   deterministically routed to Worker A.
//! - **Worker B — the survivor.** A normal `LiminalActivityWorker` that executes
//!   the activity and replies, registered for the SAME pool.
//!
//! The proof (`failover_redispatches_to_survivor_fast`):
//!
//! 1. A real durable outbox row is driven by the REAL `OutboxDispatcher` sweep
//!    loop. The dispatcher selects Worker A, pushes the dispatch, and — once
//!    Worker A closes its connection mid-flight — the liminal `PushReplyAwaiter`
//!    wakes PROMPTLY (Stage A) with the Disconnected case.
//! 2. The aion transport classifies that as the typed
//!    `ServerError::WorkerConnectionLost` (asserted via a recording dispatch
//!    decorator), DISTINCT from a slow-worker timeout.
//! 3. `handle_dispatch_error` re-arms the row for IMMEDIATE re-claim (no backoff),
//!    so the next sweep — Worker A now deregistered by `on_worker_unregistered` —
//!    selects Worker B, which executes and completes the activity.
//! 4. The whole failover happens FAST: the test asserts an upper bound comfortably
//!    below the 30s push-reply timeout, proving the row did NOT block on the full
//!    timeout (the Stage A + immediate-re-arm path, not a 30s wait).
#![cfg(feature = "liminal-transport")]

use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aion_core::{ActivityId, RunId, WorkflowId};
use aion_server::ServerError;
use aion_server::worker::{
    ConnectedWorkerRegistry, LiminalConnectionNotifier, OutboxDeliveryCallback, OutboxDispatcher,
    OutboxDispatcherConfig, OutboxRowDispatch, RegistryLiminalDispatch,
};
use aion_store::{OutboxRow, OutboxStatus, OutboxStore};
use aion_store_libsql::LibSqlStore;
use aion_worker::{ActivityRegistry, LiminalActivityWorker, WorkerConfig};
use chrono::Utc;
use liminal::protocol::WorkerRegistration;
use liminal_sdk::PushClient;
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::ConnectionSupervisor;
use liminal_server::server::listener::ServerListener;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

type TestError = Box<dyn Error + Send + Sync>;

/// One recorded completion: the correlation ids plus the worker's result string.
type CompletionRecord = (WorkflowId, ActivityId, Option<RunId>, String);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const NAMESPACE: &str = "remote";
const TASK_QUEUE: &str = "gpu";
const NODE: &str = "box-7";
const ACTIVITY_TYPE: &str = "charge-card";

/// How long the doomed worker blocks for the one dispatch it is meant to receive.
/// Generous relative to the dispatch round trip but far below the 30s push-reply
/// timeout, so the test never confuses a slow handoff with a hang.
const DOOMED_RECV_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound for the whole failover. The server-side push-reply timeout is 30s;
/// a failover that completes well under that proves the row did NOT wait out the
/// full timeout (Stage A wake + immediate re-arm), but instead failed over fast.
/// 12s leaves ample slack for CI jitter while staying comfortably below 30s.
const FAILOVER_DEADLINE: Duration = Duration::from_secs(12);

/// The activity input/output the survivor handler round-trips, proving Worker B
/// genuinely executed the failed-over dispatch.
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
/// `ServerOutboxDeliveryCallback`: records each completion so the test can assert
/// the survivor's result re-entered aion through the shared seam exactly once.
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

/// Wraps the real [`RegistryLiminalDispatch`] and records every dispatch error so
/// the test can assert the FIRST failure was the typed connection-lost
/// classification (not a slow-worker timeout, not a no-worker selection error).
struct RecordingDispatch {
    inner: RegistryLiminalDispatch,
    errors: Mutex<Vec<ServerError>>,
    successes: AtomicUsize,
}

impl RecordingDispatch {
    const fn new(inner: RegistryLiminalDispatch) -> Self {
        Self {
            inner,
            errors: Mutex::new(Vec::new()),
            successes: AtomicUsize::new(0),
        }
    }

    fn first_error_is_connection_lost(&self) -> Result<bool, TestError> {
        let errors = self
            .errors
            .lock()
            .map_err(|_| test_error("dispatch error log poisoned"))?;
        let classification = errors
            .first()
            .ok_or_else(|| test_error("no dispatch error was recorded"))?
            .is_worker_connection_lost();
        drop(errors);
        Ok(classification)
    }

    fn success_count(&self) -> usize {
        self.successes.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl OutboxRowDispatch for RecordingDispatch {
    async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError> {
        match self.inner.dispatch(row).await {
            Ok(()) => {
                self.successes.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            Err(error) => {
                if let Ok(mut errors) = self.errors.lock() {
                    errors.push(clone_error(&error));
                }
                Err(error)
            }
        }
    }
}

/// Clones the subset of [`ServerError`] this test records. Only the
/// connection-lost vs other distinction is asserted, so the recorded copy
/// preserves the variant faithfully and renders other variants by their display
/// text — enough to assert the classification without `ServerError: Clone`.
fn clone_error(error: &ServerError) -> ServerError {
    if error.is_worker_connection_lost() {
        ServerError::worker_connection_lost("liminal-push", error.to_string())
    } else {
        ServerError::worker_dispatch("liminal", "liminal-push", error.to_string())
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
        };

        let registry = ConnectedWorkerRegistry::default();
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

    /// Returns how many workers the registry currently routes to for the test pool.
    fn worker_count(&self) -> Result<usize, TestError> {
        // The registry exposes single-worker selection; count by repeatedly
        // selecting is not possible, so the test waits on selection presence and
        // verifies pool membership transitions through `select_worker`.
        let selected = self
            .registry
            .select_worker(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, Some(NODE))
            .map_err(test_error)?;
        Ok(usize::from(selected.is_some()))
    }

    /// Waits until at least one worker is registered for the test pool.
    fn wait_for_worker(&self) -> Result<(), TestError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if self.worker_count()? > 0 {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(test_error("server never registered a worker for the pool"))
    }

    fn shutdown(mut self) -> Result<(), TestError> {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().map_err(test_error)?;
        }
        Ok(())
    }
}

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

/// The in-band registration the doomed worker announces, identical in pool
/// addressing to the survivor so both land in the SAME `(ns, tq, node)` pool.
fn doomed_registration() -> WorkerRegistration {
    WorkerRegistration {
        namespaces: vec![NAMESPACE.to_owned()],
        task_queue: TASK_QUEUE.to_owned(),
        node: Some(NODE.to_owned()),
        activity_types: vec![ACTIVITY_TYPE.to_owned()],
        identity: "lsub3-doomed-worker".to_owned(),
    }
}

/// The doomed worker, on its own OS thread: a raw liminal `PushClient` that
/// registers in-band, receives exactly ONE pushed dispatch, signals receipt, then
/// drops the client — closing the socket WITHOUT a correlated reply, which is the
/// mid-dispatch kill the failover hinges on.
struct DoomedWorker {
    received: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DoomedWorker {
    fn spawn(address: String) -> Self {
        let received = Arc::new(AtomicBool::new(false));
        let thread_received = Arc::clone(&received);
        let handle = std::thread::spawn(move || {
            let client =
                match PushClient::connect_with_registration(&address, doomed_registration()) {
                    Ok(client) => client,
                    Err(error) => {
                        eprintln!("doomed worker connect failed: {error}");
                        return;
                    }
                };
            // Block for the ONE dispatch this worker is meant to receive, mark it
            // received, then fall out of scope — dropping `client` closes the
            // socket WITHOUT replying. That is the deterministic mid-dispatch kill:
            // the server's push-reply awaiter wakes (Stage A) with Disconnected.
            match client.recv_timeout(DOOMED_RECV_TIMEOUT) {
                Ok(_frame) => thread_received.store(true, Ordering::SeqCst),
                Err(error) => eprintln!("doomed worker never received a dispatch: {error}"),
            }
            drop(client);
        });
        Self {
            received,
            handle: Some(handle),
        }
    }

    fn received_dispatch(&self) -> bool {
        self.received.load(Ordering::SeqCst)
    }

    fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().ok();
        }
    }
}

/// Builds the survivor's activity registry, counting executions so the test can
/// assert Worker B genuinely ran the failed-over activity exactly once.
fn survivor_registry(executions: Arc<AtomicUsize>) -> Result<Arc<ActivityRegistry>, TestError> {
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

fn survivor_config() -> Result<WorkerConfig, TestError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(NAMESPACE)
        .task_queue(TASK_QUEUE)
        .node(NODE)
        .identity("lsub3-survivor-worker")
        .max_concurrency(1)
        .reconnect_initial_backoff(Duration::from_millis(5))
        .reconnect_max_backoff(Duration::from_millis(20))
        .reconnect_max_attempts(3)
        .build()
        .map_err(test_error)
}

/// The survivor worker on a dedicated OS thread with its own current-thread
/// runtime (the push receive is blocking), stopped via the returned flag.
struct SurvivorWorker {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SurvivorWorker {
    fn spawn(address: String, config: WorkerConfig, registry: Arc<ActivityRegistry>) -> Self {
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
                let worker = match LiminalActivityWorker::connect(&address, &config, registry) {
                    Ok(worker) => worker,
                    Err(error) => {
                        eprintln!("survivor connect failed: {error}");
                        return;
                    }
                };
                if let Err(error) = worker
                    .serve_until(|| thread_stop.load(Ordering::SeqCst))
                    .await
                {
                    eprintln!("survivor serve loop ended with error: {error}");
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

/// Builds a pending outbox row for the test pool.
fn pending_row() -> Result<OutboxRow, TestError> {
    let workflow_id = WorkflowId::new(Uuid::new_v4());
    let dispatch_key = format!("{workflow_id}:0");
    let input =
        aion_core::Payload::from_json(&serde_json::json!({ "amount": 42 })).map_err(test_error)?;
    Ok(OutboxRow {
        dispatch_key,
        workflow_id,
        ordinal: 0,
        run_id: Some(RunId::new(Uuid::new_v4())),
        namespace: NAMESPACE.to_owned(),
        task_queue: TASK_QUEUE.to_owned(),
        node: Some(NODE.to_owned()),
        activity_type: ACTIVITY_TYPE.to_owned(),
        input,
        status: OutboxStatus::Pending,
        attempt: 0,
        visible_after: Utc::now(),
        claimed_at: None,
    })
}

async fn open_store() -> Result<Arc<LibSqlStore>, TestError> {
    let nanos = Instant::now().elapsed().as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aion-lsub3-failover-{}-{nanos}.db",
        std::process::id()
    ));
    LibSqlStore::open(path)
        .await
        .map(Arc::new)
        .map_err(test_error)
}

/// Dispatcher config tuned for a fast test sweep: a short poll interval so the
/// re-armed row is re-claimed promptly, and a backoff far longer than the test's
/// failover deadline so that IF the connection-lost path were (wrongly) routed
/// through backoff, the test would TIME OUT rather than pass — the immediate
/// re-arm is therefore load-bearing for the test, not incidental.
const fn dispatcher_config() -> OutboxDispatcherConfig {
    OutboxDispatcherConfig {
        poll_interval: Duration::from_millis(25),
        batch_size: 16,
        max_attempts: 5,
        // 60s base backoff: a backed-off retry would not become claimable within
        // the 12s deadline, so only the immediate re-arm can make this test pass.
        backoff_base: Duration::from_secs(60),
        backoff_multiplier: 2,
        backoff_max: Duration::from_secs(120),
    }
}

/// Polls the row state until it reaches `Done` or the [`FAILOVER_DEADLINE`] since
/// `started` elapses. Returns whether the row completed within the deadline.
async fn wait_for_done(
    store: &LibSqlStore,
    dispatch_key: &str,
    started: Instant,
) -> Result<bool, TestError> {
    while started.elapsed() < FAILOVER_DEADLINE {
        let done = store
            .outbox_row_state(dispatch_key)
            .await
            .map_err(test_error)?
            .map(|state| state.status)
            == Some(OutboxStatus::Done);
        if done {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(false)
}

/// Asserts the survivor recorded EXACTLY ONE terminal completion, correlated to
/// the dispatched row, with the handler's real output — the doomed worker, which
/// never replied, contributes none.
fn assert_single_survivor_completion(
    callback: &RecordingCallback,
    row: &OutboxRow,
) -> Result<(), TestError> {
    let completion = {
        let completions = callback
            .completions
            .lock()
            .map_err(|_| test_error("completions lock poisoned"))?;
        assert_eq!(
            completions.len(),
            1,
            "exactly one terminal completion (from the survivor)"
        );
        completions
            .first()
            .cloned()
            .ok_or_else(|| test_error("no completion recorded"))?
    };
    let (workflow_id, activity_id, run_id, result) = completion;
    assert_eq!(&workflow_id, &row.workflow_id);
    assert_eq!(
        &activity_id,
        &ActivityId::from_sequence_position(row.ordinal)
    );
    assert_eq!(
        &run_id, &row.run_id,
        "run_id survived the failover round trip"
    );
    let output: ChargeOutput = serde_json::from_str(&result).map_err(test_error)?;
    assert!(output.charged, "the survivor handler ran and charged");
    assert_eq!(output.amount, 42, "the survivor saw the dispatched input");
    Ok(())
}

/// THE LSUB-3 PROOF: kill Worker A mid-dispatch and watch the row fail over fast
/// to Worker B through the real `OutboxDispatcher` re-arm path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failover_redispatches_to_survivor_fast() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address.to_string();

    // Worker A (the doomed worker) registers FIRST, so it takes the lowest worker
    // id and `select_worker` deterministically routes the first dispatch to it.
    let doomed = DoomedWorker::spawn(address.clone());
    server.wait_for_worker()?;

    // Worker B (the survivor) joins the SAME pool. With both present, the pool has
    // two members; A (lowest id) is selected first, B is the failover target.
    let executions = Arc::new(AtomicUsize::new(0));
    let survivor_acts = survivor_registry(Arc::clone(&executions))?;
    let survivor = SurvivorWorker::spawn(address.clone(), survivor_config()?, survivor_acts);
    // Wait until BOTH workers are connected: A is already registered, so we wait
    // for the registration count to reach the point where killing A still leaves
    // a live B. The registry's single-select API cannot count to two directly, so
    // we instead confirm B's connection is established by waiting for the survivor
    // to be serving — proven below by its successful execution after failover.
    std::thread::sleep(Duration::from_millis(100));

    // Stage one pending row in a real durable outbox and drive it with the REAL
    // OutboxDispatcher: claim -> select A -> push -> A dies -> connection-lost ->
    // immediate re-arm -> re-claim -> select B -> execute -> complete.
    let store = open_store().await?;
    let row = pending_row()?;
    store
        .append_outbox_batch(std::slice::from_ref(&row))
        .await
        .map_err(test_error)?;

    let callback = Arc::new(RecordingCallback::default());
    let registry_dispatch = RegistryLiminalDispatch::new(
        server.registry.clone(),
        Arc::clone(&callback) as Arc<dyn OutboxDeliveryCallback>,
    );
    let dispatch = Arc::new(RecordingDispatch::new(registry_dispatch));
    let dispatcher = OutboxDispatcher::new(
        store.clone(),
        Arc::clone(&dispatch) as Arc<dyn OutboxRowDispatch>,
        dispatcher_config(),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let started = Instant::now();
    let loop_handle = tokio::spawn(dispatcher.run(shutdown_rx));

    // Wait for the row to reach Done — the survivor completed the failed-over
    // dispatch — within the deadline that is comfortably below the 30s push-reply
    // timeout. If the row had blocked on the full push timeout, or if the
    // connection-lost path had (wrongly) used the 60s backoff, this would not
    // complete in time and the test would fail.
    let completed = wait_for_done(store.as_ref(), &row.dispatch_key, started).await?;
    let elapsed = started.elapsed();

    // Stop the dispatcher loop cleanly before asserting.
    shutdown_tx.send(true).map_err(test_error)?;
    tokio::time::timeout(Duration::from_secs(5), loop_handle)
        .await
        .map_err(|_| test_error("dispatcher loop did not stop after shutdown"))?
        .map_err(test_error)?;

    // (d) FAST: the whole failover completed well under the 30s push timeout.
    assert!(
        completed,
        "the row never reached Done within {FAILOVER_DEADLINE:?} (elapsed {elapsed:?}); \
         a 30s-push-timeout block or a backed-off retry would cause this"
    );
    assert!(
        elapsed < FAILOVER_DEADLINE,
        "failover took {elapsed:?}, expected well under {FAILOVER_DEADLINE:?} (< 30s push timeout)"
    );

    // The doomed worker genuinely received the first dispatch (the kill was
    // mid-dispatch, not pre-dispatch).
    assert!(
        doomed.received_dispatch(),
        "the doomed worker must have received the first dispatch before closing"
    );

    // (a) The first dispatch failure was the typed connection-lost classification,
    // NOT a slow-worker timeout or a no-worker selection error.
    assert!(
        dispatch.first_error_is_connection_lost()?,
        "the first dispatch error must be the WorkerConnectionLost classification"
    );

    // (b)+(c) The row was re-dispatched to the survivor, which executed exactly
    // once and completed it. Exactly one terminal completion is recorded, and the
    // doomed worker (which never replied) produced none.
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the survivor must have executed the failed-over activity exactly once"
    );
    assert_eq!(
        dispatch.success_count(),
        1,
        "exactly one dispatch (to the survivor) must have succeeded"
    );
    assert_single_survivor_completion(&callback, &row)?;

    survivor.stop();
    doomed.join();
    server.shutdown()?;
    Ok(())
}
