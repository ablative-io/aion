//! NOI-5b + NOI-6 tails: the LIVE agent execute path over real liminal TCP.
//!
//! Gated on `liminal-transport`, so a default build never compiles it. It stands up
//! a REAL `liminal-server` over loopback with the aion in-band registration notifier
//! WIRED TO A REAL TRANSCRIPT SEQUENCER (the NOI-5b observability tap) and a REAL
//! `AttemptOwnerIndex`, connects a REAL remote `aion-worker`
//! (`LiminalActivityWorker`) carrying a fake `AgentHarness` registered as an agent
//! activity type, and PUSHES a real `DispatchRequest`. Unlike the NOI-6 e2e (which
//! manually calls `spawn_agent` + `owners.bind`), here the WORKER EXECUTE PATH itself
//! drives `spawn_dyn_agent`, installs the live event drain, and self-registers the
//! session — the whole point of the two tails.
//!
//! The proofs (the 5 gates that map to the live path):
//!
//! - GATE 1 (live mid-run streaming): the session emits transcript events which reach
//!   the server's `O`-keyspace and the live transcript broadcast MID-RUN — asserted
//!   durable/served BEFORE the activity completes (the drain drains at event
//!   boundaries, not at exit).
//! - GATE 2 (intervention to the self-registered session): an `InjectMessage` and a
//!   `Cancel` routed through the server's `InterventionRouter` reach the live session
//!   the execute path registered and are applied; an unadvertised `PauseResume` is
//!   refused at the server (capability gate) and never reaches the session.
//! - GATE 3 (replay-invisibility): the observability events never enter the workflow
//!   replay stream — asserted here via the transcript publisher's durable tail vs the
//!   (absent) engine history, extending the NOI-5 negative control to the live path.
//! - GATE 4 (resume-by-`store_seq`): a late reader resumes the durable transcript from
//!   a `store_seq` with no gap and no duplicate, on the live-streamed events.
#![cfg(feature = "liminal-transport")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aion_core::{
    ActivityEvent, ActivityEventKind, ActivityId, ContentType, InjectPriority,
    InterventionCapabilities, InterventionCommand, InterventionKind, InterventionOutcome,
    InterventionPrimitive, MessageRole, Payload, WorkflowId,
};
use aion_integrations::contract::{AgentHarness, AgentSession};
use aion_integrations::error::HarnessError;
use aion_integrations::spec::AgentRunSpec;
use aion_server::activity_publisher::TranscriptStreamLagged;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig as ServerWorkerConfig,
};
use aion_server::worker::{AttemptKey, LiminalConnectionNotifier};
use aion_server::{
    NamespaceResolver, ServerState, StaticScheduleNamespaces, StaticWorkflowNamespaces,
};
use aion_store::ActivityStreamKey;
use aion_worker::{
    ActivityRegistry, AgentHarnessConfig, LiminalActivityWorker, RedialTiming, WorkerConfig,
    serve_with_redial,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::ConnectionSupervisor;
use liminal_server::server::listener::ServerListener;
use tokio::sync::mpsc;
use uuid::Uuid;

type TestError = Box<dyn std::error::Error + Send + Sync>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const NAMESPACE: &str = "default";
const TASK_QUEUE: &str = "default";
const ACTIVITY_TYPE: &str = "agent";
const ORDINAL: u64 = 3;

fn test_error(message: impl std::fmt::Display) -> TestError {
    message.to_string().into()
}

fn workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::nil())
}

// --- The live fake agent session: streams events, then blocks the terminal result
//     on a release gate so the test can assert MID-RUN before the run completes. ----

struct FakeSession {
    capabilities: InterventionCapabilities,
    applied: Arc<Mutex<Vec<InterventionKind>>>,
    events: Option<mpsc::UnboundedReceiver<ActivityEvent>>,
    /// Held until the test releases the run: `wait_result` awaits it, so the session
    /// stays LIVE (registered, interruptible, streaming) until the test says so.
    release: Arc<tokio::sync::Notify>,
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
        match self.events.take() {
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
        // Block the terminal result until the test releases the run, so the session
        // stays live while the test asserts mid-run streaming + intervention.
        self.release.notified().await;
        Ok(Payload::new(ContentType::Json, b"\"done\"".to_vec()))
    }
}

/// Build a message event for the fixed stream (`store_seq` stamped by the server).
fn message_event(worker_seq: u64, text: &str) -> ActivityEvent {
    ActivityEvent {
        workflow_id: workflow_id(),
        activity_id: ActivityId::from_sequence_position(ORDINAL),
        attempt: 1,
        agent_id: Uuid::from_u128(9),
        agent_role: "orchestrator".to_owned(),
        emitted_at: chrono::Utc::now(),
        worker_seq,
        store_seq: None,
        ephemeral: false,
        kind: ActivityEventKind::Message {
            role: MessageRole::Assistant,
            text: text.to_owned(),
        },
    }
}

// --- Server harness (real liminal TCP), wired to a ServerState so the transcript tap
//     + attempt-owner index + intervention router all share one set of handles. -----

struct RunningServer {
    listener: Option<ServerListener>,
    state: ServerState,
    address: SocketAddr,
}

impl RunningServer {
    fn start(capabilities: InterventionCapabilities) -> Result<Self, TestError> {
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
        let state = ServerState::from_parts(resolver, runtime_config());

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
            // liminal 0.3.0: no WebSocket listener, participant capability
            // disabled — byte-identical to the pre-0.3.0 build, matching run.rs.
            websocket: None,
            participant: None,
        };
        // The notifier shares the SAME registry, transcript sequencer, and (via the
        // dispatch binder) attempt-owner index as the ServerState, so the whole live
        // path — registration, transcript tap, ownership — is one coherent set.
        let notifier = Arc::new(
            LiminalConnectionNotifier::new(state.worker_registry().clone())
                .with_intervention_capabilities(capabilities)
                .with_transcript_publisher(state.transcript_publisher().clone()),
        );
        let supervisor = build_supervisor_with_notifier(&config, notifier.clone())?;
        if !notifier.bind_supervisor(supervisor.clone()) {
            return Err(test_error("notifier supervisor was already bound"));
        }
        let listener = ServerListener::bind(&config, supervisor).map_err(test_error)?;
        let address = listener.local_addr();
        Ok(Self {
            listener: Some(listener),
            state,
            address,
        })
    }

    fn wait_for_registered_worker(&self) -> Result<aion_server::worker::WorkerId, TestError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(handle) = self
                .state
                .worker_registry()
                .select_worker(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, None)
                .map_err(test_error)?
            {
                return Ok(handle.id());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(test_error("server never registered the in-band worker"))
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

fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        listen: ListenConfig {
            grpc: SocketAddr::from(([127, 0, 0, 1], 0)),
            http: SocketAddr::from(([127, 0, 0, 1], 0)),
        },
        tls: None,
        auth: AuthConfig {
            enabled: false,
            jwks_url: None,
            jwks_refresh_seconds: 300,
        },
        ops_console: OpsConsoleConfig {
            source: OpsConsoleAssetSource::Embedded,
        },
        namespace: NamespaceConfig {
            mode: NamespaceMode::SharedEngine,
        },
        worker: ServerWorkerConfig {
            heartbeat_window: Duration::from_millis(30_000),
        },
        websocket: WebSocketConfig {
            outbound_buffer_bound: 32,
            event_broadcast_capacity: Some(64),
            cluster_broadcast_capacity: Some(64),
        },
        workflow_packages: Vec::new(),
        deploy: DeployConfig::default(),
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        outbox: aion_server::config::OutboxConfig::default(),
        observability: aion_server::config::ObservabilityConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

fn worker_config() -> Result<WorkerConfig, TestError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(NAMESPACE)
        .task_queue(TASK_QUEUE)
        .node("")
        .identity("noi5b-worker")
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

fn inject(attempt: u32) -> InterventionCommand {
    InterventionCommand {
        workflow_id: workflow_id(),
        activity_id: ActivityId::from_sequence_position(ORDINAL),
        attempt,
        issued_by: Some("operator".to_owned()),
        issued_at: chrono::Utc::now(),
        kind: InterventionKind::InjectMessage {
            text: "stop editing that file".to_owned(),
            priority: InjectPriority::Interrupt,
        },
    }
}

fn stream_key() -> ActivityStreamKey {
    ActivityStreamKey::new(
        workflow_id(),
        ActivityId::from_sequence_position(ORDINAL),
        1,
    )
}

/// THE LOAD-BEARING LIVE TEST: dispatch an agent activity over REAL liminal so the
/// worker execute path self-registers the session + streams the transcript; then
/// assert mid-run durability, intervention to the live session, capability gate,
/// replay-invisibility, and resume-by-store_seq.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_agent_streams_transcript_and_takes_intervention() -> Result<(), TestError> {
    let capabilities = InterventionCapabilities::from_primitives([
        InterventionPrimitive::InjectMessage,
        InterventionPrimitive::Cancel,
    ]);
    let server = RunningServer::start(capabilities.clone())?;
    let address = server.address.to_string();

    // The live session's plumbing: an events channel the test feeds, an applied-log,
    // and a release gate holding the terminal result open.
    let (events_tx, events_rx) = mpsc::unbounded_channel::<ActivityEvent>();
    let applied = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(tokio::sync::Notify::new());
    let harness = Arc::new(FakeHarness {
        session: Mutex::new(Some(FakeSession {
            capabilities: capabilities.clone(),
            applied: Arc::clone(&applied),
            events: Some(events_rx),
            release: Arc::clone(&release),
        })),
    });

    // Subscribe to the LIVE transcript broadcast BEFORE the run, so we can prove the
    // event is delivered live (mid-run), not only persisted.
    let mut live = server
        .state
        .transcript_publisher()
        .subscribe(stream_key(), None);

    // Connect the worker with the agent harness bound to ACTIVITY_TYPE, on its own
    // thread (the push loop is blocking), and serve.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_address = address.clone();
    let worker_caps = capabilities.clone();
    let worker_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("worker runtime builds");
        runtime.block_on(async move {
            let registry = agent_registry().expect("agent registry");
            let config = worker_config().expect("worker config");
            let worker = LiminalActivityWorker::connect(&worker_address, &config, registry)
                .expect("worker connects")
                .with_agent_harness(harness, [ACTIVITY_TYPE], worker_caps);
            worker
                .serve_until(|| worker_stop.load(Ordering::SeqCst))
                .await
                .expect("worker serve loop");
        });
    });

    let worker_id = server.wait_for_registered_worker()?;

    // Bind the owner exactly as the dispatch path does (the dispatch bind is unit-
    // tested separately), so the router resolves this attempt to the live worker.
    let attempt_key = AttemptKey::new(
        workflow_id(),
        ActivityId::from_sequence_position(ORDINAL),
        1,
    );
    server
        .state
        .attempt_owners()
        .bind(attempt_key.clone(), worker_id);

    // Push a real DispatchRequest to the worker over liminal; the worker's execute
    // path routes it to the agent harness, self-registers the session, and starts the
    // event drain. Run the blocking push off-thread (it does not return until the run
    // is released, so it stays parked until we complete the session at the end).
    let dispatch_state = server.state.clone();
    let push_task = tokio::task::spawn_blocking(move || push_dispatch(&dispatch_state, worker_id));

    // GATE 1: the session emits a transcript event the instant it starts; it must
    // reach the O-keyspace AND the live broadcast MID-RUN (the run is still blocked on
    // `release`). Receiving it live also proves the session started + the drain is
    // wired, so registration (which precedes the run in `execute_agent`) is complete.
    events_tx
        .send(message_event(1, "thinking..."))
        .map_err(test_error)?;
    let live_event = tokio::time::timeout(CONNECT_TIMEOUT, next_event(&mut live))
        .await
        .map_err(|_| test_error("no live transcript event arrived mid-run"))??;
    assert_eq!(live_event.store_seq, Some(0), "commit-allocated store_seq");
    assert!(matches!(
        live_event.kind,
        ActivityEventKind::Message { text, .. } if text == "thinking..."
    ));
    // Durable mid-run: the O-keyspace already holds it BEFORE the run completes.
    let durable_midrun = server
        .state
        .transcript_publisher()
        .replay_from(&stream_key(), 0)
        .await
        .map_err(test_error)?;
    assert_eq!(
        durable_midrun.len(),
        1,
        "event durable mid-run, before completion"
    );

    // GATE 2: an InjectMessage + Cancel reach the self-registered live session, and an
    // unadvertised PauseResume is refused at the server (never reaching the session).
    assert_interventions_applied_and_gated(&server, &applied).await?;

    // Emit a durable tail while the run is still live, then assert resume-by-store_seq
    // (GATE 4) and replay-invisibility (GATE 3).
    events_tx
        .send(message_event(2, "m-1"))
        .map_err(test_error)?;
    events_tx
        .send(message_event(3, "m-2"))
        .map_err(test_error)?;
    assert_durable_tail_and_replay_invisibility(&server).await?;

    // End the run and assert the finished attempt is the too-late no-op.
    drop(events_tx);
    release.notify_one();
    assert_finished_attempt_is_stale(&server).await?;

    // The parked push returns now that the run completed; join it so nothing leaks.
    let _ = push_task.await;
    stop.store(true, Ordering::SeqCst);
    worker_thread.join().ok();
    server.shutdown()?;
    Ok(())
}

/// GATE 2: route an `InjectMessage` + `Cancel` to the live self-registered session
/// (both `Applied`), then an unadvertised `PauseResume` (server-gated), and assert the
/// session's applied-log holds EXACTLY the two accepted commands in order.
async fn assert_interventions_applied_and_gated(
    server: &RunningServer,
    applied: &Arc<Mutex<Vec<InterventionKind>>>,
) -> Result<(), TestError> {
    let router = server.state.intervention_router();
    assert_eq!(
        router.route(inject(1)).await.map_err(test_error)?,
        InterventionOutcome::Applied
    );
    let cancel = InterventionCommand {
        kind: InterventionKind::Cancel {
            reason: "operator abort".to_owned(),
        },
        ..inject(1)
    };
    assert_eq!(
        router.route(cancel).await.map_err(test_error)?,
        InterventionOutcome::Applied
    );
    let gated = InterventionCommand {
        kind: InterventionKind::PauseResume { paused: true },
        ..inject(1)
    };
    assert!(matches!(
        router.route(gated).await.map_err(test_error)?,
        InterventionOutcome::CapabilityNotSupported {
            primitive: InterventionPrimitive::PauseResume
        }
    ));
    let log = applied.lock().unwrap();
    assert_eq!(log.len(), 2, "only the two advertised commands applied");
    assert!(matches!(log[0], InterventionKind::InjectMessage { .. }));
    assert!(matches!(log[1], InterventionKind::Cancel { .. }));
    Ok(())
}

/// GATE 4 + GATE 3: wait for the mid-run tail to persist, assert a late reader resumes
/// from `store_seq` 1 with exactly the missed records (no gap/dup), and assert the
/// events are observability records only (never the replay engine's history).
async fn assert_durable_tail_and_replay_invisibility(
    server: &RunningServer,
) -> Result<(), TestError> {
    let publisher = server.state.transcript_publisher();
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    // 4 records: "thinking..." (0), the retained operator inject (1, the lane
    // #229 tee on GATE 2's applied InjectMessage), then m-1 (2) and m-2 (3).
    while publisher
        .replay_from(&stream_key(), 0)
        .await
        .map_err(test_error)?
        .len()
        < 4
    {
        if Instant::now() > deadline {
            return Err(test_error("durable tail did not reach 4 records"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // GATE 4: resume from store_seq 1 replays exactly store_seq 1,2,3 (no gap,
    // no dup).
    let resumed = publisher
        .replay_from(&stream_key(), 1)
        .await
        .map_err(test_error)?;
    let seqs: Vec<u64> = resumed.iter().map(|r| r.store_seq).collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "resume-by-store_seq: no gap, no duplicate"
    );
    // The lane #229 live-path wiring proof: GATE 2's applied InjectMessage was
    // teed into the durable stream as an operator User message at store_seq 1.
    let operator_record = &resumed[0].event;
    assert_eq!(operator_record.agent_role, "operator");
    assert!(
        matches!(
            &operator_record.kind,
            ActivityEventKind::Message {
                role: MessageRole::User,
                text,
            } if text == "stop editing that file"
        ),
        "the retained operator inject must be a User message with the injected text: {operator_record:?}"
    );
    // GATE 3: the events are observability records only. Byte-level O-vs-E disjointness
    // is proven at the store layer (`aion-store-haematite/tests/observability.rs`);
    // here the live path routed the transcript exclusively through the sequencer, and
    // this resolver-only state has no replay engine to receive them.
    let records = publisher
        .replay_from(&stream_key(), 0)
        .await
        .map_err(test_error)?;
    assert!(
        records.iter().all(|r| !r.event.ephemeral),
        "the durable transcript holds only non-ephemeral observability events"
    );
    assert!(
        server.state.engine().is_err(),
        "resolver-only state has no replay engine, so no replay stream received the events"
    );
    Ok(())
}

/// The finished attempt is the too-late no-op: the session deregistered on run
/// completion (the `SessionGuard` dropped), so a command to it NACKs stale-target.
async fn assert_finished_attempt_is_stale(server: &RunningServer) -> Result<(), TestError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        let outcome = server
            .state
            .intervention_router()
            .route(inject(1))
            .await
            .map_err(test_error)?;
        if matches!(outcome, InterventionOutcome::StaleTarget { .. }) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(test_error(format!(
                "a command to the finished attempt must be the too-late no-op, got {outcome:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Push a `DispatchRequest` to `worker_id` over its liminal connection, mirroring the
/// production dispatch push (`LiminalWorkerDelivery::dispatch`).
fn push_dispatch(state: &ServerState, worker_id: aion_server::worker::WorkerId) {
    use aion_server::worker::{DispatchRequest, WorkerDelivery};
    let handle = state
        .worker_registry()
        .worker_by_id(worker_id)
        .expect("registry")
        .expect("worker");
    let WorkerDelivery::Liminal(delivery) = handle.delivery() else {
        panic!("worker is not liminal-delivered");
    };
    let request = DispatchRequest {
        activity_type: ACTIVITY_TYPE.to_owned(),
        workflow_id: workflow_id(),
        ordinal: ORDINAL,
        run_id: None,
        input: b"\"in\"".to_vec(),
        attempt: 1,
        labels: std::collections::BTreeMap::new(),
        heartbeat_window_ms: 0,
    };
    // The push blocks for the reply (which arrives only when the run is released), so
    // this runs on a spawned blocking task and its result is not awaited here.
    let _ = delivery.dispatch(&request);
}

/// THE PRODUCTION-SERVE WIRING TEST (NOI-5b/NOI-6): prove the reconnect-to-survivor
/// serve entrypoint (`serve_with_redial`) — the library seam a real worker binary
/// installs — actually INSTALLS the composed agent harness, by
/// serving through an EMPTY typed registry and asserting an agent dispatch still runs
/// the live agent path (streams a transcript event) instead of failing with no
/// handler.
///
/// This is specific wiring, not a smoke test: the worker is built through
/// `serve_with_redial` with `Some(AgentHarnessConfig)` and NOTHING in its typed
/// registry. If `serve_with_redial` did NOT thread the harness through (the bug this
/// closes), the agent activity would route to the empty registry, fail
/// missing-handler, and emit NO transcript event — so the mid-run transcript
/// assertion below fails. It passes ONLY because the served worker drives the
/// installed harness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_with_redial_installs_the_composed_harness() -> Result<(), TestError> {
    let capabilities = InterventionCapabilities::from_primitives([
        InterventionPrimitive::InjectMessage,
        InterventionPrimitive::Cancel,
    ]);
    let server = RunningServer::start(capabilities.clone())?;
    let address = server.address.to_string();

    let (events_tx, events_rx) = mpsc::unbounded_channel::<ActivityEvent>();
    let applied = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(tokio::sync::Notify::new());
    let harness: Arc<dyn aion_integrations::contract::DynAgentHarness> = Arc::new(FakeHarness {
        session: Mutex::new(Some(FakeSession {
            capabilities: capabilities.clone(),
            applied: Arc::clone(&applied),
            events: Some(events_rx),
            release: Arc::clone(&release),
        })),
    });

    let mut live = server
        .state
        .transcript_publisher()
        .subscribe(stream_key(), None);

    // Drive the PRODUCTION serve entrypoint with the harness bundled as an
    // `AgentHarnessConfig` — the exact shape a real worker binary's composition root
    // passes — over an EMPTY typed registry (no handler for ACTIVITY_TYPE).
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_address = address.clone();
    let agent = AgentHarnessConfig::new(harness, [ACTIVITY_TYPE], capabilities.clone());
    let worker_thread = std::thread::spawn(move || {
        let empty_registry = Arc::new(ActivityRegistry::new());
        let config = worker_config().expect("worker config");
        serve_with_redial(
            vec![worker_address],
            &config,
            &empty_registry,
            RedialTiming::new(Duration::from_millis(5), Duration::from_millis(20)),
            &worker_stop,
            Some(&agent),
            || {},
        )
        .expect("serve_with_redial loop");
    });

    let worker_id = server.wait_for_registered_worker()?;
    let attempt_key = AttemptKey::new(
        workflow_id(),
        ActivityId::from_sequence_position(ORDINAL),
        1,
    );
    server.state.attempt_owners().bind(attempt_key, worker_id);

    let dispatch_state = server.state.clone();
    let push_task = tokio::task::spawn_blocking(move || push_dispatch(&dispatch_state, worker_id));

    // THE PROOF: the harness (installed by `serve_with_redial`) ran, so its transcript
    // event reaches the live broadcast mid-run. An unwired harness would have routed to
    // the empty registry and produced no event here.
    events_tx
        .send(message_event(1, "installed via serve_with_redial"))
        .map_err(test_error)?;
    let live_event = tokio::time::timeout(CONNECT_TIMEOUT, next_event(&mut live))
        .await
        .map_err(|_| {
            test_error("no live transcript event — serve_with_redial did not install the harness")
        })??;
    assert!(
        matches!(
            live_event.kind,
            ActivityEventKind::Message { text, .. } if text == "installed via serve_with_redial"
        ),
        "the served worker drove the installed agent harness, not the empty registry"
    );

    drop(events_tx);
    release.notify_one();
    let _ = push_task.await;
    stop.store(true, Ordering::SeqCst);
    worker_thread.join().ok();
    server.shutdown()?;
    Ok(())
}

/// Read the next event from a transcript subscription stream.
async fn next_event(
    stream: &mut futures::stream::BoxStream<'static, Result<ActivityEvent, TranscriptStreamLagged>>,
) -> Result<ActivityEvent, TestError> {
    use futures::StreamExt;
    match stream.next().await {
        Some(Ok(event)) => Ok(event),
        Some(Err(lag)) => Err(test_error(format!("transcript lagged: {lag}"))),
        None => Err(test_error("transcript stream closed")),
    }
}
