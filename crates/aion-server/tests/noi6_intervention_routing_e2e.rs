//! NOI-6: mid-run intervention routing end-to-end.
//!
//! Gated on `liminal-transport`, so a default build never compiles it. It stands
//! up a REAL `liminal-server` over loopback with the aion in-band registration
//! notifier, connects a REAL remote `aion-worker` (`LiminalActivityWorker`), and
//! drives the WHOLE intervention path with real components:
//!
//! - a live agent session (a fake `AgentSession` that records applied commands,
//!   driven by the real `spawn_agent` trait driver) is registered into the
//!   worker's `ControlRegistry` under its `(workflow, activity, attempt)` key, and
//!   the server binds that attempt's owner in its `AttemptOwnerIndex`;
//! - the server `InterventionRouter` (with the production `LiminalInterventionTransport`)
//!   GATES on the worker's advertised capabilities, RESOLVES the owning worker,
//!   and PUSHES the command over liminal;
//! - the worker's serve loop demuxes the push, delivers it to the live session via
//!   `session.intervene()`, and replies the neutral ack;
//!
//! The proof:
//!
//! - `operator_command_routes_gates_and_applies` — an `InjectMessage` and a
//!   `Cancel` route through the server, over liminal, and are applied by the live
//!   session, each returning `Applied`. The session recorded both.
//! - negative control (a): an UNADVERTISED primitive is refused at the SERVER and
//!   NEVER reaches the worker (the session records nothing for it).
//! - negative control (b): a command for a stale/nonexistent attempt returns the
//!   app-range too-late no-op, not a panic.
#![cfg(feature = "liminal-transport")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::error::Error;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aion_core::{
    ActivityEvent, ActivityId, ContentType, InjectPriority, InterventionCapabilities,
    InterventionCommand, InterventionKind, InterventionOutcome, InterventionPrimitive, Payload,
    WorkflowId,
};
use aion_integrations::contract::{AgentHarness, AgentSession};
use aion_integrations::error::HarnessError;
use aion_integrations::spec::AgentRunSpec;
use aion_server::worker::{
    AttemptKey, AttemptOwnerIndex, ConnectedWorkerRegistry, InterventionRouter,
    LiminalConnectionNotifier, LiminalInterventionTransport,
};
use aion_worker::{
    ActivityRegistry, ControlRegistry, LiminalActivityWorker, SessionKey, WorkerConfig, spawn_agent,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::ConnectionSupervisor;
use liminal_server::server::listener::ServerListener;
use tokio::sync::mpsc;
use uuid::Uuid;

type TestError = Box<dyn Error + Send + Sync>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const NAMESPACE: &str = "default";
const TASK_QUEUE: &str = "default";
const ACTIVITY_TYPE: &str = "agent";

fn test_error(message: impl std::fmt::Display) -> TestError {
    message.to_string().into()
}

// --- The live fake agent session (records applied interventions) -------------

struct FakeSession {
    capabilities: InterventionCapabilities,
    applied: Arc<Mutex<Vec<InterventionKind>>>,
    end_events: Option<mpsc::UnboundedReceiver<ActivityEvent>>,
}

struct FakeHarness {
    session: Mutex<Option<FakeSession>>,
}

#[async_trait]
impl AgentHarness for FakeHarness {
    type Session = FakeSession;
    async fn start(&self, _spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        self.session
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| HarnessError::transport("started twice"))
    }
}

#[async_trait]
impl AgentSession for FakeSession {
    fn capabilities(&self) -> &InterventionCapabilities {
        &self.capabilities
    }
    fn events(&mut self) -> BoxStream<'static, ActivityEvent> {
        match self.end_events.take() {
            Some(rx) => Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(rx)),
            None => Box::pin(futures::stream::empty()),
        }
    }
    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError> {
        if !self.capabilities.supports(&cmd.kind) {
            return Err(HarnessError::capability_not_supported("gated"));
        }
        self.applied.lock().unwrap().push(cmd.kind);
        Ok(())
    }
    async fn wait_result(self) -> Result<Payload, HarnessError> {
        Ok(Payload::new(ContentType::Json, b"null".to_vec()))
    }
}

/// A live session registered on `control`, driven by a real `spawn_agent`. Holding
/// `end_events` keeps the run alive; dropping it ends the run.
struct LiveSession {
    applied: Arc<Mutex<Vec<InterventionKind>>>,
    _guard: aion_worker::SessionGuard,
    end_events: mpsc::UnboundedSender<ActivityEvent>,
    driver: tokio::task::JoinHandle<()>,
}

fn spawn_live_session(
    control: &ControlRegistry,
    key: &SessionKey,
    capabilities: InterventionCapabilities,
) -> LiveSession {
    let applied = Arc::new(Mutex::new(Vec::new()));
    let (end_events, events_rx) = mpsc::unbounded_channel();
    let session = FakeSession {
        capabilities: capabilities.clone(),
        applied: Arc::clone(&applied),
        end_events: Some(events_rx),
    };
    let harness = FakeHarness {
        session: Mutex::new(Some(session)),
    };
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let guard = control.register(key.clone(), control_tx, capabilities);
    let spec = AgentRunSpec::new(
        key.workflow_id.clone(),
        key.activity_id.clone(),
        key.attempt,
        Payload::new(ContentType::Json, b"\"in\"".to_vec()),
    );
    let driver = tokio::spawn(async move {
        let _ = spawn_agent(&harness, spec, event_tx, Some(control_rx)).await;
    });
    LiveSession {
        applied,
        _guard: guard,
        end_events,
        driver,
    }
}

// --- The liminal server + worker harness (mirrors lsub1_xnode_dispatch_e2e) ---

struct RunningServer {
    listener: Option<ServerListener>,
    registry: ConnectedWorkerRegistry,
    address: SocketAddr,
}

impl RunningServer {
    fn start(capabilities: InterventionCapabilities) -> Result<Self, TestError> {
        let config = ServerConfig {
            listen_address: "127.0.0.1:0".parse().map_err(test_error)?,
            health_listen_address: reserve_loopback_port()?,
            channels: Vec::<ChannelDef>::new(),
            routing_rules: Vec::new(),
            persistence_path: None,
            cluster: None,
            drain_timeout_ms: 30_000,
        };
        let registry = ConnectedWorkerRegistry::default();
        // The liminal-connected agent worker advertises the harness's neutral
        // capability set at registration (NOI-6 item 4): the notifier stamps it onto
        // the worker's handle, where the router gates on it.
        let notifier = Arc::new(
            LiminalConnectionNotifier::new(registry.clone())
                .with_intervention_capabilities(capabilities),
        );
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

    fn wait_for_registered_worker(&self) -> Result<(), TestError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if self
                .registry
                .select_worker(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, None)
                .map_err(test_error)?
                .is_some()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(test_error("server never registered the in-band worker"))
    }

    fn worker_id(&self) -> Result<aion_server::worker::WorkerId, TestError> {
        self.registry
            .select_worker(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, None)
            .map_err(test_error)?
            .map(|handle| handle.id())
            .ok_or_else(|| test_error("no worker registered"))
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

fn worker_config() -> Result<WorkerConfig, TestError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(NAMESPACE)
        .task_queue(TASK_QUEUE)
        .node("")
        .identity("noi6-worker")
        .max_concurrency(1)
        .reconnect_initial_backoff(Duration::from_millis(5))
        .reconnect_max_backoff(Duration::from_millis(20))
        .reconnect_max_attempts(3)
        .build()
        .map_err(test_error)
}

fn agent_registry() -> Result<Arc<ActivityRegistry>, TestError> {
    let registry = ActivityRegistry::new()
        .register_activity(ACTIVITY_TYPE, |_input: serde_json::Value, _ctx| {
            Box::pin(async move { Ok(serde_json::json!({})) })
        })
        .map_err(test_error)?;
    Ok(Arc::new(registry))
}

/// The worker thread: connects, hands its `ControlRegistry` back to the test, then
/// serves the push loop until stopped.
struct WorkerThread {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl WorkerThread {
    fn spawn(
        address: String,
        config: WorkerConfig,
        registry: Arc<ActivityRegistry>,
        control_out: std::sync::mpsc::Sender<ControlRegistry>,
    ) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("worker runtime builds");
            runtime.block_on(async move {
                let worker = match LiminalActivityWorker::connect(&address, &config, registry) {
                    Ok(worker) => worker,
                    Err(error) => {
                        eprintln!("worker connect failed: {error}");
                        return;
                    }
                };
                // Hand the worker's control back-index to the test so it can register
                // a live session on the SAME instance the serve loop routes through.
                let _ = control_out.send(worker.control_registry().clone());
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

fn keys() -> (SessionKey, AttemptKey) {
    let workflow_id = WorkflowId::new(Uuid::nil());
    let activity_id = ActivityId::from_sequence_position(3);
    (
        SessionKey::new(workflow_id.clone(), activity_id.clone(), 1),
        AttemptKey::new(workflow_id, activity_id, 1),
    )
}

fn command(activity: &ActivityId, attempt: u32, kind: InterventionKind) -> InterventionCommand {
    InterventionCommand {
        workflow_id: WorkflowId::new(Uuid::nil()),
        activity_id: activity.clone(),
        attempt,
        issued_by: Some("operator".to_owned()),
        issued_at: chrono::Utc::now(),
        kind,
    }
}

fn inject(activity: &ActivityId, attempt: u32) -> InterventionCommand {
    command(
        activity,
        attempt,
        InterventionKind::InjectMessage {
            text: "stop editing that file, use the other module".to_owned(),
            priority: InjectPriority::Interrupt,
        },
    )
}

/// THE LOAD-BEARING TEST: operator -> server gate + route -> liminal push ->
/// worker apply -> ack, for an `InjectMessage` and a `Cancel`, plus the two negative
/// controls (unadvertised gated at server; stale attempt = too-late no-op).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operator_command_routes_gates_and_applies() -> Result<(), TestError> {
    // The worker's harness advertises {InjectMessage, Cancel}; the notifier stamps
    // this onto the worker handle at registration, and the router gates on it.
    let capabilities = InterventionCapabilities::from_primitives([
        InterventionPrimitive::InjectMessage,
        InterventionPrimitive::Cancel,
    ]);
    let server = RunningServer::start(capabilities.clone())?;
    let address = server.address.to_string();

    // Connect a real worker and take its control back-index.
    let (control_tx, control_rx) = std::sync::mpsc::channel();
    let worker = WorkerThread::spawn(address, worker_config()?, agent_registry()?, control_tx);
    server.wait_for_registered_worker()?;
    let control = control_rx
        .recv_timeout(CONNECT_TIMEOUT)
        .map_err(|_| test_error("worker never handed back its control registry"))?;
    let worker_id = server.worker_id()?;

    // A live session on the worker's control back-index + the server owner binding.
    let (session_key, attempt_key) = keys();
    let live = spawn_live_session(&control, &session_key, capabilities.clone());

    let owners = AttemptOwnerIndex::new();
    // Bind the owner to the LIMINAL-delivered worker (the one that can be pushed to).
    owners.bind(attempt_key.clone(), worker_id);
    let router = InterventionRouter::new(
        server.registry.clone(),
        owners.clone(),
        Arc::new(LiminalInterventionTransport),
    );

    // (1) An InjectMessage routes, gates OK, is pushed over liminal, applied.
    let outcome = router.route(inject(&session_key.activity_id, 1)).await?;
    assert_eq!(
        outcome,
        InterventionOutcome::Applied,
        "an advertised InjectMessage must apply end-to-end"
    );

    // (2) A Cancel likewise routes and applies.
    let cancel = command(
        &session_key.activity_id,
        1,
        InterventionKind::Cancel {
            reason: "operator abort".to_owned(),
        },
    );
    assert_eq!(router.route(cancel).await?, InterventionOutcome::Applied);

    // The live session recorded BOTH applied commands, in order.
    {
        let applied = live.applied.lock().unwrap();
        assert_eq!(
            applied.len(),
            2,
            "both commands were applied by the session"
        );
        assert!(matches!(applied[0], InterventionKind::InjectMessage { .. }));
        assert!(matches!(applied[1], InterventionKind::Cancel { .. }));
    }

    // Negative control (a): an UNADVERTISED primitive (PauseResume) is refused at
    // the SERVER and NEVER pushed — the session records nothing new.
    let gated = command(
        &session_key.activity_id,
        1,
        InterventionKind::PauseResume { paused: true },
    );
    let gated_outcome = router.route(gated).await?;
    assert!(
        matches!(
            gated_outcome,
            InterventionOutcome::CapabilityNotSupported {
                primitive: InterventionPrimitive::PauseResume
            }
        ),
        "an unadvertised primitive must be gated at the server, got {gated_outcome:?}"
    );
    assert_eq!(
        live.applied.lock().unwrap().len(),
        2,
        "a server-gated command must never reach the session"
    );

    // Negative control (b): a command for a stale/nonexistent attempt (attempt 2 has
    // no owner bound) returns the app-range too-late no-op, not a panic.
    let stale = inject(&session_key.activity_id, 2);
    let stale_outcome = router.route(stale).await?;
    assert!(
        matches!(stale_outcome, InterventionOutcome::StaleTarget { .. }),
        "a command for an unowned attempt must be the too-late no-op, got {stale_outcome:?}"
    );

    // Tear down: end the session run, join the driver, stop the worker + server.
    drop(live.end_events);
    let _ = live.driver.await;
    worker.stop();
    server.shutdown()?;
    Ok(())
}
