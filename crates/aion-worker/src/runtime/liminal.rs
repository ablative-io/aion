//! Liminal worker transport: receive pushed dispatches, execute, reply (LSUB-1).
//!
//! # What this is (bounded spike)
//!
//! This module is the SUBSCRIBER half of the cross-node work-dispatch path the
//! server's [`liminal_transport`] module dispatches into. Behind the
//! `liminal-transport` Cargo feature, a [`LiminalActivityWorker`] connects a
//! server-push client to a liminal server, then runs a serve loop: it receives a
//! server-pushed [`DispatchRequest`], executes the named activity through the
//! EXISTING [`ActivityRegistry`](crate::ActivityRegistry) (the same execution
//! path the gRPC worker uses), and answers with a correlated [`DispatchResponse`]
//! on the same connection. The default worker build (no feature) is byte-identical
//! and never links liminal.
//!
//! [`liminal_transport`]: https://docs.rs/aion-server
//!
//! # The transport it composes (LSUB-0 server push)
//!
//! Liminal's LSUB-0 primitive is a SERVER-INITIATED push: the server writes a
//! `Frame::Push` (correlation id + opaque payload) on the client's existing
//! connection, and the client answers with a correlated `Frame::PushReply`. The
//! SDK side is [`liminal_sdk::PushClient`]: a background reader thread surfaces
//! each pushed frame on a channel ([`PushClient::recv_timeout`]), and the caller
//! sends the correlated reply with [`PushClient::reply`]. This worker drives that
//! loop synchronously on a dedicated blocking thread (the push client is
//! thread-based, not async), executing each activity on a Tokio runtime handle.
//!
//! # Wire contract (must match the server byte-for-byte)
//!
//! The server side serializes its `DispatchRequest`/`DispatchResponse` (in
//! `aion-server`'s `liminal_transport`) through serde JSON. This module mirrors
//! those structs field-for-field with the SAME serde field names and the SAME
//! `aion-core` id types ([`WorkflowId`], [`RunId`]), so the JSON on the wire is
//! identical. The two crates cannot share one struct (the worker must not depend
//! on the server), so the contract is pinned by the shared field set and a wire
//! round-trip test here; any divergence is a wire-compatibility break.
//!
//! # In-band self-registration (LSUB-L2)
//!
//! The worker is SELF-DESCRIBING over the socket. [`LiminalActivityWorker::connect`]
//! builds a [`liminal::protocol::WorkerRegistration`] from the worker's
//! [`WorkerConfig`] (its `namespaces`, `task_queue`, `node`, `identity`) and the
//! activity-type names it binds in its [`ActivityRegistry`], then connects via the
//! SDK's `connect_with_registration`: a synchronous `WorkerRegister` ->
//! `WorkerRegisterAck` round-trip runs before the push reader spawns. The server's
//! installed connection-notifier turns that into a first-class connected-worker
//! registry membership, so the worker is selected the SAME way a gRPC worker is —
//! retiring the LSUB-1 out-of-band `active_connection_pids()` + hard-coded
//! server-side registration. A `Rejected` ack surfaces as a connect error
//! (the rejection reason is carried), so a worker the server declines never
//! believes it is registered.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use aion_core::{
    ActivityEvent, ActivityId, ContentType, InterventionCapabilities, InterventionCommand,
    InterventionOutcome, Payload, RunId, WorkflowId,
};
use aion_integrations::contract::DynAgentHarness;
use aion_integrations::spec::AgentRunSpec;
use liminal::protocol::WorkerRegistration;
use liminal_sdk::{OBSERVABILITY_CHANNEL, PushClient, PushWriter, PushedFrame};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::activity::ActivityRegistry;
use crate::config::WorkerConfig;
use crate::context::ActivityContext;
use crate::error::WorkerError;
use crate::protocol::ActivityTask;
use crate::runtime::agent::spawn_dyn_agent;
use crate::runtime::intervention::{ControlRegistry, SessionKey};
use crate::runtime::liminal_redial::ServeResult;
use crate::runtime::loop_::{ActivityDispatcher, DispatchOutcome};

/// Wire request carrying one scheduled activity from the server to this worker.
///
/// Field-for-field mirror of `aion-server`'s `liminal_transport::DispatchRequest`
/// (same serde field names + `aion-core` id types), so the JSON the server pushes
/// deserializes here unchanged. See the module docs for the cross-crate contract.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispatchRequest {
    /// Activity type this worker must execute.
    pub activity_type: String,
    /// Workflow that scheduled this fan-out activity.
    pub workflow_id: WorkflowId,
    /// Pinned ordinal of this activity within the workflow's fan-out range.
    pub ordinal: u64,
    /// Run that dispatched this ordinal, when known (continue-as-new safety).
    pub run_id: Option<RunId>,
    /// Opaque activity input bytes (JSON-tagged on the aion side).
    pub input: Vec<u8>,
}

/// Wire response carrying this worker's result back to the server.
///
/// Field-for-field mirror of `aion-server`'s
/// `liminal_transport::DispatchResponse`, so the server's `LiminalCompletionSource`
/// re-enters it unchanged.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispatchResponse {
    /// Workflow the completion belongs to.
    pub workflow_id: WorkflowId,
    /// Pinned ordinal the completion correlates against.
    pub ordinal: u64,
    /// Run that issued the dispatch, echoed back for the run gate.
    pub run_id: Option<RunId>,
    /// Worker outcome: `Ok(result)` or `Err(reason)`.
    pub outcome: Result<String, String>,
}

/// Wire request carrying one neutral mid-run intervention command from the server
/// to this worker (NOI-6).
///
/// Field-for-field mirror of `aion-server`'s
/// `liminal_transport::InterventionRequest`. It rides the SAME server-push channel
/// as [`DispatchRequest`], distinguished by its unique required `intervention`
/// field — a [`DispatchRequest`] has none, so the serve loop demuxes the two by
/// which one deserializes. The envelope is neutral: it carries an
/// [`InterventionCommand`], never a harness type.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterventionRequest {
    /// The neutral command to deliver to the session owning the target attempt.
    pub intervention: InterventionCommand,
}

/// Wire reply carrying this worker's neutral intervention ack back to the server
/// (NOI-6). Field-for-field mirror of `aion-server`'s
/// `liminal_transport::InterventionReply`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterventionReply {
    /// The neutral applied/gated/stale outcome the operator receives.
    pub outcome: InterventionOutcome,
}

/// How long the serve loop blocks for the next server push before re-checking the
/// shutdown flag. A bounded poll lets [`LiminalActivityWorker::serve_until`] stop
/// promptly on a quiet connection rather than blocking forever.
const RECV_POLL: Duration = Duration::from_millis(100);

/// The composed agent harness a served worker drives, plus the agent activity
/// types it owns and the neutral [`InterventionCapabilities`] it advertises
/// (NOI-5b/NOI-6).
///
/// This bundles the three things [`LiminalActivityWorker::with_agent_harness`]
/// needs into one `Option`-shaped value so the production serve path
/// ([`serve_with_redial`](crate::serve_with_redial)) can thread a composed harness
/// through as a single argument — or `None` for a harness-less build, which serves
/// non-agent activities exactly as before. The harness is ERASED
/// (`Arc<dyn DynAgentHarness>`), so no concrete adapter type ever appears in this
/// platform crate.
#[derive(Clone)]
pub struct AgentHarnessConfig {
    /// The erased agent harness the worker drives for its agent activity types.
    harness: Arc<dyn DynAgentHarness>,
    /// The activity-type names routed through the harness rather than the registry.
    agent_activity_types: BTreeSet<String>,
    /// The neutral intervention primitives the harness advertises.
    capabilities: InterventionCapabilities,
}

impl AgentHarnessConfig {
    /// Builds a config from a composed (erased) `harness`, the `agent_activity_types`
    /// it owns, and the `capabilities` it advertises.
    #[must_use]
    pub fn new(
        harness: Arc<dyn DynAgentHarness>,
        agent_activity_types: impl IntoIterator<Item = impl Into<String>>,
        capabilities: InterventionCapabilities,
    ) -> Self {
        Self {
            harness,
            agent_activity_types: agent_activity_types.into_iter().map(Into::into).collect(),
            capabilities,
        }
    }

    /// The agent activity-type names this config owns — the set the serve path must
    /// ADVERTISE in registration so the server can select the worker for them.
    #[must_use]
    pub fn agent_activity_types(&self) -> &BTreeSet<String> {
        &self.agent_activity_types
    }
}

impl std::fmt::Debug for AgentHarnessConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentHarnessConfig")
            .field("agent_activity_types", &self.agent_activity_types)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

/// A worker that serves activities over the liminal server-push transport.
///
/// Construct with [`LiminalActivityWorker::connect`], then drive the serve loop
/// with [`LiminalActivityWorker::serve_until`] (loops until the stop flag) or
/// [`LiminalActivityWorker::serve_one`] (handles exactly one pushed dispatch,
/// used by tests and single-shot callers). The activity registry is the SAME
/// typed registry the gRPC worker executes through.
pub struct LiminalActivityWorker {
    client: PushClient,
    registry: Arc<ActivityRegistry>,
    /// The attempt back-index a pushed intervention is routed through (NOI-6). A
    /// pushed [`InterventionRequest`] is delivered to the live session owning its
    /// target `(workflow, activity, attempt)`; a command with no live owner is the
    /// attempt-scoped stale-target no-op. Shared (an `Arc` inside) with the
    /// session-spawn path that registers each running agent session.
    control: ControlRegistry,
    /// Optional agent harness this worker drives for its agent activity types
    /// (NOI-5b/NOI-6). When installed via [`Self::with_agent_harness`], an activity
    /// whose type is in [`Self::agent_activity_types`] is executed by driving the
    /// harness through [`spawn_dyn_agent`] — streaming its transcript live and
    /// self-registering the live session — instead of the plain typed registry. A
    /// worker without a harness (the default) is byte-identical to before.
    agent_harness: Option<Arc<dyn DynAgentHarness>>,
    /// The activity-type names executed through the agent harness rather than the
    /// plain registry. Empty unless [`Self::with_agent_harness`] installs a harness.
    agent_activity_types: BTreeSet<String>,
    /// The neutral intervention primitives the installed agent harness advertises —
    /// declared at construction (mirroring the server-side notifier's
    /// `with_intervention_capabilities`), because capabilities are needed to register
    /// the session in the [`ControlRegistry`] BEFORE the session starts.
    agent_capabilities: InterventionCapabilities,
}

impl std::fmt::Debug for LiminalActivityWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiminalActivityWorker")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl LiminalActivityWorker {
    /// Connects a server-push client to `address`, SELF-REGISTERS in-band, and
    /// starts its background reader, binding this worker's typed activity registry.
    ///
    /// The registration is built from `config` (its `namespaces`, `task_queue`,
    /// `node`, `identity`) and the activity-type names bound in `registry`, then
    /// driven through the SDK's `connect_with_registration`: the
    /// `WorkerRegister` -> `WorkerRegisterAck` round-trip completes synchronously
    /// before the push reader spawns. The server's connection-notifier turns the
    /// accepted registration into a connected-worker registry membership.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError::Transport`] when the push connection, handshake, or
    /// registration fails — INCLUDING a server-side `Rejected` registration, whose
    /// reason is carried in the error so the worker never serves while unregistered.
    pub fn connect(
        address: &str,
        config: &WorkerConfig,
        registry: Arc<ActivityRegistry>,
    ) -> Result<Self, WorkerError> {
        Self::connect_advertising(address, config, registry, &BTreeSet::new())
    }

    /// Connects like [`Self::connect`] but ADDITIONALLY advertises `agent_types` in
    /// the in-band registration (NOI-5b/NOI-6).
    ///
    /// An agent activity is driven by the installed harness, not the typed registry,
    /// so its type never appears in `registry.activity_types()`. Without advertising
    /// it here, the server could not SELECT this worker for that activity — a worker
    /// whose only activities are agent-driven would register advertising nothing. So
    /// the registration announces `registry`'s types UNION `agent_types`; the union is
    /// what makes an agent-only worker selectable. Used by the production serve path
    /// ([`serve_with_redial`](crate::serve_with_redial)) so a redialed worker
    /// re-advertises its agent types on every connection.
    ///
    /// # Errors
    ///
    /// Same as [`Self::connect`]: [`WorkerError::Transport`] on a failed connect,
    /// handshake, or registration.
    pub fn connect_advertising(
        address: &str,
        config: &WorkerConfig,
        registry: Arc<ActivityRegistry>,
        agent_types: &BTreeSet<String>,
    ) -> Result<Self, WorkerError> {
        let mut registration = registration_from(config, &registry);
        for agent_type in agent_types {
            if !registration.activity_types.contains(agent_type) {
                registration.activity_types.push(agent_type.clone());
            }
        }
        let client = PushClient::connect_with_registration(address, registration)
            .map_err(|error| transport_error(&error))?;
        Ok(Self {
            client,
            registry,
            control: ControlRegistry::new(),
            agent_harness: None,
            agent_activity_types: BTreeSet::new(),
            agent_capabilities: InterventionCapabilities::none(),
        })
    }

    /// Install an agent `harness` this worker drives for the given
    /// `agent_activity_types`, advertising `capabilities` (NOI-5b/NOI-6).
    ///
    /// An activity whose type is in `agent_activity_types` is executed by driving the
    /// harness through [`spawn_dyn_agent`]: its transcript streams LIVE to the server
    /// over this worker's connection, and the live session self-registers in the
    /// [`ControlRegistry`] under its `(workflow, activity, attempt)` key so a pushed
    /// intervention reaches it. `capabilities` is the harness's advertised neutral
    /// primitive set (the same set the server-side notifier advertises for this
    /// worker), gated on before a command is delivered. Every other activity type
    /// runs through the plain typed registry exactly as before; a worker built
    /// without this builder never touches the agent path.
    #[must_use]
    pub fn with_agent_harness(
        mut self,
        harness: Arc<dyn DynAgentHarness>,
        agent_activity_types: impl IntoIterator<Item = impl Into<String>>,
        capabilities: InterventionCapabilities,
    ) -> Self {
        self.agent_harness = Some(harness);
        self.agent_activity_types = agent_activity_types.into_iter().map(Into::into).collect();
        self.agent_capabilities = capabilities;
        self
    }

    /// Install the optional agent harness carried by an [`AgentHarnessConfig`], if
    /// one is supplied (NOI-5b/NOI-6).
    ///
    /// This is the single seam the production serve path
    /// ([`serve_with_redial`](crate::serve_with_redial)) threads a composed harness
    /// through: `Some(config)` applies it via [`Self::with_agent_harness`], `None`
    /// leaves the worker on the plain typed-registry path exactly as before — so a
    /// harness-less build (`--no-default-features`) is unaffected. Kept distinct from
    /// [`Self::with_agent_harness`] so the redial driver can carry the harness as one
    /// `Option`-shaped value.
    #[must_use]
    pub fn with_agent_config(self, config: Option<AgentHarnessConfig>) -> Self {
        match config {
            Some(config) => self.with_agent_harness(
                config.harness,
                config.agent_activity_types,
                config.capabilities,
            ),
            None => self,
        }
    }

    /// Whether `activity_type` is driven through the installed agent harness rather
    /// than the plain typed registry — `true` only when a harness is installed AND
    /// the type is registered as an agent type.
    ///
    /// The public form of the internal routing predicate ([`Self::is_agent_activity`]),
    /// exposed so a caller (and the wiring tests) can assert a served worker actually
    /// routes an agent type to the agent path.
    #[must_use]
    pub fn drives_agent_activity(&self, activity_type: &str) -> bool {
        self.is_agent_activity(activity_type)
    }

    /// The intervention control back-index this worker routes pushed commands
    /// through (NOI-6). The session-spawn path registers each running agent session
    /// here so a routed intervention reaches the live attempt that owns it.
    #[must_use]
    pub fn control_registry(&self) -> &ControlRegistry {
        &self.control
    }

    /// Blocks up to `RECV_POLL` for the next pushed dispatch, executes it, and
    /// replies. Returns `Ok(true)` when one dispatch was served, `Ok(false)` when
    /// the poll elapsed with no push (so the caller can re-check a stop flag).
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] when a push frame cannot be decoded, the activity
    /// reply cannot be encoded, or the reply cannot be written to the socket.
    pub async fn serve_one(&self) -> Result<bool, WorkerError> {
        match self.client.recv_timeout(RECV_POLL) {
            Ok(frame) => {
                self.handle_pushed_frame(frame).await?;
                Ok(true)
            }
            // A bare timeout with no push is not an error: surface it as "nothing
            // served" so the serve loop can re-check its stop flag. Any other
            // receive error (the reader stopped, the server closed) is fatal.
            Err(error) if is_recv_timeout(&error) => Ok(false),
            Err(error) => Err(transport_error(&error)),
        }
    }

    /// Serves pushed dispatches until `stop` returns `true`.
    ///
    /// Re-checks `stop` every [`RECV_POLL`], so a caller can stop the worker
    /// promptly even on a quiet connection.
    ///
    /// # Errors
    ///
    /// Returns the first [`WorkerError`] a served dispatch surfaces (decode,
    /// encode, or transport).
    pub async fn serve_until<Stop>(&self, mut stop: Stop) -> Result<(), WorkerError>
    where
        Stop: FnMut() -> bool + Send,
    {
        while !stop() {
            self.serve_one().await?;
        }
        Ok(())
    }

    /// Serves pushed dispatches until `stop` fires (a clean stop) or the
    /// connection drops with a transport error (the owner died), reporting which
    /// occurred and whether any dispatch was served on this connection.
    ///
    /// This is the per-connection step the candidate-cycling redial driver
    /// (`serve_with_redial`) runs: a clean stop ends the worker, a drop tells the
    /// driver to redial the next candidate, and `served_work` lets the driver
    /// reset its backoff after a connection that did useful work.
    pub(crate) async fn serve_until_drop<Stop>(&self, mut stop: Stop) -> ServeResult
    where
        Stop: FnMut() -> bool + Send,
    {
        let mut served_work = false;
        while !stop() {
            match self.serve_one().await {
                Ok(true) => served_work = true,
                Ok(false) => {}
                // A transport drop (the connected server died) is the redial
                // trigger, not a fatal worker error: surface it so the driver
                // migrates to the next candidate and re-registers there.
                Err(_) => return ServeResult::Dropped { served_work },
            }
        }
        ServeResult::Stopped
    }

    /// Decodes one pushed frame and dispatches it by kind: an
    /// [`InterventionRequest`] (NOI-6) is routed to the live session owning its
    /// target attempt and answered with a correlated [`InterventionReply`]; anything
    /// else is a [`DispatchRequest`] executed as an activity and answered with a
    /// [`DispatchResponse`].
    ///
    /// The two share the push channel and are demuxed by which one deserializes: an
    /// [`InterventionRequest`] has a unique required `intervention` field a
    /// [`DispatchRequest`] lacks, so a dispatch frame never decodes as an
    /// intervention and vice-versa. Intervention is tried first; on a miss the frame
    /// is decoded as a dispatch (preserving the existing dispatch path exactly).
    async fn handle_pushed_frame(&self, frame: PushedFrame) -> Result<(), WorkerError> {
        let correlation_id = frame.correlation_id();
        if let Ok(request) = serde_json::from_slice::<InterventionRequest>(frame.payload()) {
            let outcome = self.control.deliver(request.intervention).await;
            let reply = InterventionReply { outcome };
            let payload = serde_json::to_vec(&reply).map_err(WorkerError::encode)?;
            return self
                .client
                .reply(correlation_id, payload)
                .map_err(|error| transport_error(&error));
        }
        let request: DispatchRequest =
            serde_json::from_slice(frame.payload()).map_err(WorkerError::decode)?;
        // An AGENT dispatch may run for a long time. It MUST NOT block the serve loop,
        // or a mid-run intervention push (which rides the SAME channel) could never be
        // received. So it is SPAWNED: the run drives concurrently and replies its own
        // correlated DispatchResponse when it finishes, while the serve loop returns to
        // receive interventions. A plain activity runs inline (short, no live session).
        if self.is_agent_activity(&request.activity_type) {
            self.spawn_agent_dispatch(correlation_id, request);
            return Ok(());
        }
        let response = self.execute(&request).await?;
        let payload = serde_json::to_vec(&response).map_err(WorkerError::encode)?;
        self.client
            .reply(correlation_id, payload)
            .map_err(|error| transport_error(&error))
    }

    /// Spawns an agent dispatch as a background task so the serve loop stays free to
    /// receive mid-run interventions, replying its own correlated [`DispatchResponse`]
    /// when the run completes.
    ///
    /// The task holds only cheap clones (the harness `Arc`, the shared
    /// [`ControlRegistry`], and a [`PushWriter`] reply/drain leg of the connection), so
    /// it outlives the borrow of `&self`. The session self-registers + streams its
    /// transcript inside [`run_agent_dispatch`].
    fn spawn_agent_dispatch(&self, correlation_id: u64, request: DispatchRequest) {
        let Some(harness) = self.agent_harness.clone() else {
            return;
        };
        let control = self.control.clone();
        let capabilities = self.agent_capabilities.clone();
        let writer = self.client.writer_handle();
        // Run on a DEDICATED thread with its own current-thread runtime, not
        // `tokio::spawn`: the erased agent session drives a `?Send` future (the neutral
        // `AgentSession` is `Send` but not `Sync`), which `tokio::spawn` cannot accept.
        // `block_on` has no `Send` bound, and every captured handle (the harness `Arc`,
        // the shared `ControlRegistry`, the `PushWriter`) IS `Send`, so the future is
        // built and driven entirely on the new thread and never crosses one. The
        // session self-registers so the serve loop's intervention pushes still reach it.
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                tracing::warn!("agent dispatch: failed to build runtime for the agent run");
                return;
            };
            runtime.block_on(run_agent_dispatch(
                harness,
                control,
                capabilities,
                writer,
                correlation_id,
                request,
            ));
        });
    }

    /// Executes one dispatch and maps its outcome onto a [`DispatchResponse`].
    ///
    /// An activity whose type is registered as an agent type (via
    /// [`Self::with_agent_harness`]) is driven through the agent path
    /// ([`Self::execute_agent`]): its transcript streams LIVE to the server and the
    /// live session self-registers for intervention. Every other activity runs
    /// through the plain typed registry ([`Self::execute_registry`]). Either way a
    /// missing handler / failure becomes a failure outcome (a reason string), never a
    /// dropped reply, so the server always sees a correlated answer it can re-enter.
    async fn execute(&self, request: &DispatchRequest) -> Result<DispatchResponse, WorkerError> {
        // Agent activities are spawned in `handle_pushed_frame` and never reach here;
        // this path is the plain typed-registry execution, now carrying the LIVE
        // transcript event drain so a handler that emits events streams them mid-run.
        let attempt = 1;
        let activity_id = ActivityId::from_sequence_position(request.ordinal);
        let task = ActivityTask {
            workflow_id: request.workflow_id.clone(),
            activity_id: activity_id.clone(),
            run_id: request.run_id.clone(),
            activity_type: request.activity_type.clone(),
            attempt,
            input: Payload::new(ContentType::Json, request.input.clone()),
            labels: std::collections::BTreeMap::new(),
        };
        let (event_sender, drain) = spawn_event_drain(self.client.writer_handle());
        let (context, cancellation) = ActivityContext::for_workflow_with_events(
            Some(request.workflow_id.clone()),
            activity_id,
            attempt,
            None,
            Some(event_sender),
        );
        // The push transport has no cooperative-cancellation channel in the spike;
        // drop the handle so the activity simply runs to completion.
        drop(cancellation);
        let outcome = self.registry.dispatch(task, context).await;
        drain.finish().await;
        let outcome = match outcome {
            Ok(DispatchOutcome::Completed { output }) => Ok(result_string(&output)),
            Ok(DispatchOutcome::Failed { failure }) => Err(failure.message),
            Err(error) => Err(error.to_string()),
        };
        Ok(DispatchResponse {
            workflow_id: request.workflow_id.clone(),
            ordinal: request.ordinal,
            run_id: request.run_id.clone(),
            outcome,
        })
    }

    /// Whether `activity_type` is driven through the installed agent harness.
    fn is_agent_activity(&self, activity_type: &str) -> bool {
        self.agent_harness.is_some() && self.agent_activity_types.contains(activity_type)
    }
}

/// Drives one agent activity attempt to completion and replies its correlated
/// [`DispatchResponse`] (NOI-5b/NOI-6). Runs as a SPAWNED task so the serve loop stays
/// free to receive mid-run interventions.
///
/// It self-registers the live session in the [`ControlRegistry`] under its
/// `(workflow, activity, attempt)` key BEFORE [`spawn_dyn_agent`] starts it — so a
/// pushed intervention resolves the instant the run begins — streams the session's
/// transcript LIVE over `writer`, routes pushed commands to the session, and
/// deregisters on completion (the [`SessionGuard`](crate::runtime::intervention::SessionGuard)
/// drops on every exit path). The terminal result is replied to `correlation_id`.
async fn run_agent_dispatch(
    harness: Arc<dyn DynAgentHarness>,
    control: ControlRegistry,
    capabilities: InterventionCapabilities,
    writer: PushWriter,
    correlation_id: u64,
    request: DispatchRequest,
) {
    let attempt = 1;
    let activity_id = ActivityId::from_sequence_position(request.ordinal);
    let session_key = SessionKey::new(request.workflow_id.clone(), activity_id.clone(), attempt);
    // Register the live session's control leg BEFORE the run starts; the guard
    // deregisters on drop (any exit path), so a finished attempt is never routed to.
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _session_guard = control.register(session_key, control_tx, capabilities);
    let spec = AgentRunSpec::new(
        request.workflow_id.clone(),
        activity_id,
        attempt,
        // The dispatched activity-type name is neutral run identity the harness may
        // use (e.g. to label the run); it is threaded verbatim from the request.
        request.activity_type.clone(),
        Payload::new(ContentType::Json, request.input.clone()),
    );
    // Stream the transcript LIVE over the connection while the agent runs.
    let (event_sender, drain) = spawn_event_drain(writer.clone());
    let outcome = spawn_dyn_agent(harness.as_ref(), spec, event_sender, Some(control_rx)).await;
    drain.finish().await;
    // A harness fault is a retryable activity failure, mapped as the gRPC driver does.
    let outcome =
        outcome.unwrap_or_else(|error| crate::runtime::agent::harness_error_to_outcome(&error));
    let response = DispatchResponse {
        workflow_id: request.workflow_id.clone(),
        ordinal: request.ordinal,
        run_id: request.run_id.clone(),
        outcome: match outcome {
            DispatchOutcome::Completed { output } => Ok(result_string(&output)),
            DispatchOutcome::Failed { failure } => Err(failure.message),
        },
    };
    // Reply the terminal result to the server on the shared connection. A failed
    // reply (connection gone) is logged; the outbox re-drives the row on timeout.
    match serde_json::to_vec(&response) {
        Ok(payload) => {
            if let Err(error) = writer.reply(correlation_id, payload) {
                tracing::warn!(%error, "agent dispatch: failed to reply DispatchResponse");
            }
        }
        Err(error) => tracing::warn!(%error, "agent dispatch: failed to encode DispatchResponse"),
    }
}

/// Builds the LIVE observability event drain: an [`ActivityEvent`] sender handed to
/// the [`ActivityContext`]/agent driver, and a background task that publishes every
/// event to the server over `writer` (NOI-5b).
///
/// The drain task publishes each event to [`OBSERVABILITY_CHANNEL`] as it arrives — at
/// every event boundary, MID-RUN, not batched at exit. The returned [`EventDrain`]
/// guard's [`EventDrain::finish`] drops the sender and joins the task, so the drain is
/// torn down cleanly when the activity completes.
fn spawn_event_drain(writer: PushWriter) -> (mpsc::UnboundedSender<ActivityEvent>, EventDrain) {
    let (event_sender, event_receiver) = mpsc::unbounded_channel::<ActivityEvent>();
    let handle = tokio::spawn(drain_events(writer, event_receiver));
    (event_sender, EventDrain { handle })
}

/// A running observability-drain task tied to one activity attempt.
///
/// Holding it keeps the drain alive; [`Self::finish`] drops the event sender (ending
/// the drain's receive loop) and awaits the task, so no event is lost and no task is
/// leaked across dispatches.
struct EventDrain {
    handle: tokio::task::JoinHandle<()>,
}

impl EventDrain {
    /// Await the drain task to completion. The caller has already dropped its event
    /// sender (it was moved into the context/driver), so the drain's receiver closes
    /// and the task finishes after publishing every buffered event.
    async fn finish(self) {
        drop(self.handle.await);
    }
}

/// Publishes each drained [`ActivityEvent`] to the server over the shared push
/// connection until the sender is dropped (end of the activity).
///
/// Serializes the whole neutral envelope as the frame payload (the server
/// deserializes it directly — no lossy mapping) and publishes to
/// [`OBSERVABILITY_CHANNEL`], where the server's connection-notifier tap routes it to
/// the transcript sequencer. A publish fault is logged and the drain continues: the
/// transcript is best-effort live streaming (durability is the server's O-keyspace
/// commit, not this transport), so one dropped event never fails the activity.
async fn drain_events(writer: PushWriter, mut receiver: mpsc::UnboundedReceiver<ActivityEvent>) {
    while let Some(event) = receiver.recv().await {
        let payload = match serde_json::to_vec(&event) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "observability drain: failed to encode ActivityEvent");
                continue;
            }
        };
        if let Err(error) = writer.publish(OBSERVABILITY_CHANNEL, payload) {
            tracing::warn!(%error, "observability drain: failed to publish transcript event");
        }
    }
}

/// Renders an activity output payload as the result string the server expects.
///
/// The server's `DispatchResponse.outcome` carries the success result as a
/// `String`; activity output is JSON-tagged bytes, so the UTF-8 view is the
/// result string. A non-UTF-8 payload (never produced by the JSON codec) is
/// rendered lossily rather than dropping the completion.
fn result_string(output: &Payload) -> String {
    String::from_utf8_lossy(output.bytes()).into_owned()
}

/// Whether an SDK receive error is a benign poll timeout (no push arrived) rather
/// than a fatal transport fault. [`PushClient::recv_timeout`] maps both a timeout
/// and a stopped reader to [`liminal_sdk::SdkError::Connection`]; only the timeout
/// message is non-fatal, so it is distinguished by its text.
fn is_recv_timeout(error: &liminal_sdk::SdkError) -> bool {
    error
        .to_string()
        .contains("no server push arrived within the timeout")
}

/// Builds the in-band [`WorkerRegistration`] this worker announces over the
/// socket, from its [`WorkerConfig`] routing dimensions and the activity-type
/// names bound in its [`ActivityRegistry`].
///
/// `node` follows the SAME none-convention the aion registry applies on the
/// server side: an empty `config.node` carries no locality affinity (`None`), so
/// it is semantically unpinned rather than registering a distinct empty-node
/// affinity no pinned dispatch could match; a non-empty value (the default
/// hostname, or an operator-set node) is the worker's advertised node.
fn registration_from(config: &WorkerConfig, registry: &ActivityRegistry) -> WorkerRegistration {
    let node = if config.node.is_empty() {
        None
    } else {
        Some(config.node.clone())
    };
    WorkerRegistration {
        namespaces: config.namespaces.clone(),
        task_queue: config.task_queue.clone(),
        node,
        activity_types: registry.activity_types().into_iter().collect(),
        identity: config.identity.clone(),
    }
}

/// Wraps a liminal SDK error as a retryable worker transport error.
fn transport_error(error: &liminal_sdk::SdkError) -> WorkerError {
    WorkerError::Transport {
        source: tonic::Status::unavailable(format!("liminal worker transport error: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{DispatchRequest, DispatchResponse, registration_from};
    use crate::activity::ActivityRegistry;
    use crate::config::WorkerConfig;
    use aion_core::{RunId, WorkflowId};
    use uuid::Uuid;

    fn worker_config(node: &str) -> Result<WorkerConfig, Box<dyn std::error::Error>> {
        Ok(WorkerConfig::builder()
            .endpoint("127.0.0.1:0")
            .task_queue("gpu")
            .identity("worker-a")
            .max_concurrency(1)
            .reconnect_initial_backoff(Duration::from_millis(5))
            .reconnect_max_backoff(Duration::from_millis(20))
            .reconnect_max_attempts(3)
            .namespaces([String::from("remote"), String::from("payments")])
            .node(node)
            .build()?)
    }

    fn two_activity_registry() -> Result<ActivityRegistry, Box<dyn std::error::Error>> {
        let registry = ActivityRegistry::new()
            .register_activity("charge-card", |_input: serde_json::Value, _ctx| {
                Box::pin(async move { Ok(serde_json::json!({})) })
            })?
            .register_activity("refund", |_input: serde_json::Value, _ctx| {
                Box::pin(async move { Ok(serde_json::json!({})) })
            })?;
        Ok(registry)
    }

    /// The in-band registration is built from the worker config's routing
    /// dimensions and the activity-type names the registry binds, so the worker
    /// announces exactly what it serves. The activity types come from the same
    /// registry the worker executes through (deterministic, sorted).
    #[test]
    fn registration_carries_config_and_registry_activity_types()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = worker_config("box-7")?;
        let registry = two_activity_registry()?;

        let registration = registration_from(&config, &registry);

        assert_eq!(
            registration.namespaces,
            vec![String::from("remote"), String::from("payments")]
        );
        assert_eq!(registration.task_queue, "gpu");
        assert_eq!(registration.node, Some(String::from("box-7")));
        assert_eq!(registration.identity, "worker-a");
        assert_eq!(
            registration.activity_types,
            vec![String::from("charge-card"), String::from("refund")],
            "activity types come from the bound registry, sorted"
        );
        Ok(())
    }

    /// An empty config node carries NO locality affinity (`None`), the same
    /// none-convention the server-side registry applies — a worker with no node is
    /// unpinned, not pinned to an empty node.
    #[test]
    fn registration_empty_node_is_unpinned() -> Result<(), Box<dyn std::error::Error>> {
        let config = worker_config("")?;
        let registry = two_activity_registry()?;

        let registration = registration_from(&config, &registry);

        assert_eq!(registration.node, None);
        Ok(())
    }

    /// The wire request round-trips through serde JSON with stable field names —
    /// the contract that keeps it byte-compatible with the server's struct.
    #[test]
    fn dispatch_request_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let request = DispatchRequest {
            activity_type: "charge-card".to_owned(),
            workflow_id: WorkflowId::new(Uuid::new_v4()),
            ordinal: 7,
            run_id: Some(RunId::new(Uuid::new_v4())),
            input: br#"{"amount":42}"#.to_vec(),
        };
        let bytes = serde_json::to_vec(&request)?;
        let decoded: DispatchRequest = serde_json::from_slice(&bytes)?;
        assert_eq!(decoded, request);
        // The field names the server depends on are present in the JSON.
        let json = String::from_utf8(bytes)?;
        for field in ["activity_type", "workflow_id", "ordinal", "run_id", "input"] {
            assert!(json.contains(field), "wire JSON must carry `{field}`");
        }
        Ok(())
    }

    /// An intervention request round-trips and is demuxed from a dispatch request:
    /// a dispatch JSON must NOT decode as an intervention (no `intervention` field),
    /// which is exactly what lets the serve loop tell the two pushes apart.
    #[test]
    fn intervention_request_demuxes_from_a_dispatch_request()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::{InterventionReply, InterventionRequest};
        use aion_core::{
            ActivityId, InjectPriority, InterventionCommand, InterventionKind, InterventionOutcome,
        };

        let command = InterventionCommand {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(3),
            attempt: 1,
            issued_by: Some("operator".to_owned()),
            issued_at: chrono::Utc::now(),
            kind: InterventionKind::InjectMessage {
                text: "steer".to_owned(),
                priority: InjectPriority::Interrupt,
            },
        };
        let request = InterventionRequest {
            intervention: command,
        };
        let bytes = serde_json::to_vec(&request)?;
        assert_eq!(
            serde_json::from_slice::<InterventionRequest>(&bytes)?,
            request
        );

        // A dispatch request must NOT decode as an intervention (missing field).
        let dispatch = DispatchRequest {
            activity_type: "charge-card".to_owned(),
            workflow_id: WorkflowId::new(Uuid::new_v4()),
            ordinal: 7,
            run_id: None,
            input: b"{}".to_vec(),
        };
        let dispatch_bytes = serde_json::to_vec(&dispatch)?;
        assert!(
            serde_json::from_slice::<InterventionRequest>(&dispatch_bytes).is_err(),
            "a dispatch frame must never decode as an intervention"
        );

        // The reply round-trips its neutral outcome.
        let reply = InterventionReply {
            outcome: InterventionOutcome::Applied,
        };
        let reply_bytes = serde_json::to_vec(&reply)?;
        assert_eq!(
            serde_json::from_slice::<InterventionReply>(&reply_bytes)?,
            reply
        );
        Ok(())
    }

    /// The wire response round-trips, including the `outcome` Result tagging the
    /// server's completion source matches on (`Ok`/`Err`).
    #[test]
    fn dispatch_response_round_trips_both_outcomes() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new(Uuid::new_v4());
        let ok = DispatchResponse {
            workflow_id: workflow_id.clone(),
            ordinal: 0,
            run_id: None,
            outcome: Ok(r#"{"charged":true}"#.to_owned()),
        };
        let err = DispatchResponse {
            workflow_id,
            ordinal: 1,
            run_id: None,
            outcome: Err("boom".to_owned()),
        };
        for response in [ok, err] {
            let bytes = serde_json::to_vec(&response)?;
            let decoded: DispatchResponse = serde_json::from_slice(&bytes)?;
            assert_eq!(decoded, response);
        }
        Ok(())
    }
}
