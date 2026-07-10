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
    ActivityId, ContentType, InterventionCapabilities, InterventionCommand, InterventionOutcome,
    Payload, RunId, WorkflowId,
};
use aion_integrations::contract::DynAgentHarness;
use aion_integrations::spec::AgentRunSpec;
use liminal::protocol::WorkerRegistration;
use liminal_sdk::{PushClient, PushWriter, PushedFrame};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::activity::ActivityRegistry;
use crate::config::WorkerConfig;
use crate::context::ActivityContext;
use crate::error::WorkerError;
use crate::protocol::ActivityTask;
use crate::runtime::agent::spawn_dyn_agent;
use crate::runtime::intervention::{ControlRegistry, SessionKey};
use crate::runtime::liminal_drain::{DrainBinding, LiveWriter, spawn_event_drain};
use crate::runtime::liminal_redial::{RedialBackoff, ServeResult};
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
    /// One-based delivery attempt (the gRPC `ActivityTask.attempt` mirror).
    /// Serde-defaulted to `1` so a frame from a pre-attempt server decodes as
    /// a first delivery — the exact prior behaviour.
    #[serde(default = "first_attempt")]
    pub attempt: u32,
    /// Engine-provided labels (the gRPC `ActivityTask.labels` mirror); empty
    /// on the outbox path, which has no label source.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// The server's heartbeat window in milliseconds when this dispatch is
    /// liveness-TRACKED on the server (the engine-seam bridge path), or `0`
    /// when it is not (the outbox path). Non-zero arms this worker's automatic
    /// liveness pump: beats every quarter-window on
    /// [`WORKER_LIVENESS_CHANNEL`] so the server's expiry sweeper never
    /// falsely expires a healthy long-running activity.
    #[serde(default)]
    pub heartbeat_window_ms: u64,
}

/// Serde default for [`DispatchRequest::attempt`]: a frame that predates the
/// attempt field is a first delivery.
const fn first_attempt() -> u32 {
    1
}

/// Reserved liminal channel this worker publishes automatic liveness beats on.
/// Byte-for-byte mirror of the server crate's constant of the same name (the
/// same cross-crate contract the dispatch/response mirrors pin).
pub const WORKER_LIVENESS_CHANNEL: &str = "aion.worker.liveness";

/// Wire liveness beat for one in-flight dispatch — the liminal mirror of the
/// gRPC liveness heartbeat (no progress payload). Field-for-field mirror of the
/// server crate's `WorkerLivenessBeat`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerLivenessBeat {
    /// Workflow owning the in-flight activity being kept alive.
    pub workflow_id: WorkflowId,
    /// Pinned ordinal of the in-flight activity being kept alive.
    pub ordinal: u64,
}

/// Reserved liminal channel this worker announces its intervention capabilities
/// on, immediately after each (re)registration. Byte-for-byte mirror of the
/// server crate's constant of the same name. The in-band registration frame is
/// a published liminal protocol type and cannot carry aion-level capability
/// metadata, so the announcement rides this channel instead (NOI-6).
pub const WORKER_CAPABILITIES_CHANNEL: &str = "aion.worker.capabilities";

/// Wire announcement of this worker's advertised intervention capabilities.
/// Field-for-field mirror of the server crate's `WorkerCapabilitiesAnnouncement`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerCapabilitiesAnnouncement {
    /// The neutral intervention primitives this worker's harness supports.
    pub capabilities: InterventionCapabilities,
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
    /// The shared live-connection slot every observability drain publishes
    /// through (#254). Seeded at connect with this connection's writer; the redial
    /// driver ([`serve_with_redial`](crate::serve_with_redial)) REPLACES it with a
    /// slot it refreshes on every reconnection (see [`Self::with_live_writer`]), so
    /// a drain re-resolves the survivor after a server loss instead of publishing
    /// to a dead socket forever.
    live_writer: LiveWriter,
    /// The bounded reconnect backoff the observability drain paces its re-probes
    /// with during an outage, seeded from the worker's reconnect config so the
    /// drain and the dispatch redial share one coherent schedule.
    drain_backoff: RedialBackoff,
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
        // Seed the drain slot with THIS connection's writer so a single-connection
        // serve (the direct `serve_until` callers and tests) resolves a live writer
        // immediately; the redial driver swaps in a shared, reconnect-refreshed slot
        // via `with_live_writer`. The drain backoff mirrors the dispatch reconnect
        // schedule, so both pace re-attempts identically.
        let live_writer = LiveWriter::seeded(client.writer_handle());
        let drain_backoff = RedialBackoff::new(
            config.reconnect.initial_backoff,
            config.reconnect.max_backoff,
        );
        Ok(Self {
            client,
            registry,
            control: ControlRegistry::new(),
            agent_harness: None,
            agent_activity_types: BTreeSet::new(),
            agent_capabilities: InterventionCapabilities::none(),
            live_writer,
            drain_backoff,
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

    /// Adopt `slot` as the SHARED live-connection drain slot and record this
    /// connection's writer in it (#254).
    ///
    /// The redial driver ([`serve_with_redial`](crate::serve_with_redial)) owns one
    /// slot across every reconnection and threads it into each freshly-connected
    /// worker here, so every observability drain — including one spawned against a
    /// now-dead earlier connection — re-resolves the SURVIVOR the driver most
    /// recently installed. A worker served directly (without the redial driver)
    /// keeps its own per-connection slot seeded at connect; there it simply
    /// coalesces-and-drops on a loss, there being no survivor to migrate to.
    #[must_use]
    pub(crate) fn with_live_writer(mut self, slot: LiveWriter) -> Self {
        slot.set(self.client.writer_handle());
        self.live_writer = slot;
        self
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

    /// Publish this worker's advertised intervention capabilities on the
    /// reserved [`WORKER_CAPABILITIES_CHANNEL`] (NOI-6), once per connection.
    ///
    /// A worker with no harness capabilities announces nothing: the empty set
    /// is already the server-side registration default (observability-only).
    /// A publish or encode fault is logged, never fatal — the worker still
    /// serves; the operator just sees no intervention controls until a
    /// reconnect re-announces.
    fn announce_capabilities(&self) {
        if self.agent_capabilities.supported.is_empty() {
            return;
        }
        let announcement = WorkerCapabilitiesAnnouncement {
            capabilities: self.agent_capabilities.clone(),
        };
        match serde_json::to_vec(&announcement) {
            Ok(payload) => {
                if let Err(error) = self
                    .client
                    .writer_handle()
                    .publish(WORKER_CAPABILITIES_CHANNEL, payload)
                {
                    tracing::warn!(%error, "failed to announce intervention capabilities");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to encode WorkerCapabilitiesAnnouncement");
            }
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
        self.announce_capabilities();
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
        // Once per connection: a redialed worker re-announces on the new
        // connection because the survivor's registry entry starts at the
        // registration default (observability-only).
        self.announce_capabilities();
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
        // Receipt is logged BEFORE execution so a dispatch that reaches the wrong
        // connection (or wedges mid-handler) is visible in the worker log — the
        // server side only sees silence either way (a lost-worker expiry), so this
        // line is the ground truth for "did the push arrive, and where".
        tracing::info!(
            activity_type = %request.activity_type,
            workflow_id = %request.workflow_id,
            ordinal = request.ordinal,
            attempt = request.attempt,
            serves = ?self.registry.activity_types(),
            agent_types = ?self.agent_activity_types,
            "received dispatch push"
        );
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
        // The transcript drain publishes through the SHARED live-connection slot so
        // it survives a redial, whereas the terminal reply + liveness pump stay on
        // this connection's writer (a lost reply is re-driven by the outbox).
        let drain = DrainBinding::new(self.live_writer.clone(), self.drain_backoff);
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
                drain,
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
        // The wire's one-based delivery attempt and labels are threaded through
        // verbatim (the gRPC parity contract): a retry executes with the real
        // attempt, never a re-stamped first delivery.
        let attempt = request.attempt;
        let activity_id = ActivityId::from_sequence_position(request.ordinal);
        let task = ActivityTask {
            workflow_id: request.workflow_id.clone(),
            activity_id: activity_id.clone(),
            run_id: request.run_id.clone(),
            activity_type: request.activity_type.clone(),
            attempt,
            input: Payload::new(ContentType::Json, request.input.clone()),
            labels: request.labels.clone(),
        };
        // Keep this liveness-TRACKED dispatch beating for as long as the handler
        // genuinely runs (a no-op for the window-less outbox path). The guard
        // aborts the pump on every exit path.
        let _liveness = spawn_liveness_pump(self.client.writer_handle(), request);
        let (event_sender, drain) = spawn_event_drain(DrainBinding::new(
            self.live_writer.clone(),
            self.drain_backoff,
        ));
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
            Ok(outcome) => wire_outcome(outcome),
            // A worker-level dispatch fault (e.g. a missing handler) is a
            // retryable fault: another worker in the pool may serve the type,
            // matching the gRPC contract where such a session error ends in a
            // retryable lost-worker sweep, never a terminal.
            Err(error) => Err(format!("retryable:{error}")),
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
    drain: DrainBinding,
    correlation_id: u64,
    request: DispatchRequest,
) {
    // The wire's one-based delivery attempt keys the session exactly as the
    // server binds the attempt's owner (`request_for_row` / the bridge stamp).
    let attempt = request.attempt;
    let activity_id = ActivityId::from_sequence_position(request.ordinal);
    let session_key = SessionKey::new(request.workflow_id.clone(), activity_id.clone(), attempt);
    // Register the live session's control leg BEFORE the run starts; the guard
    // deregisters on drop (any exit path), so a finished attempt is never routed to.
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _session_guard = control.register(session_key, control_tx, capabilities);
    // Keep a liveness-TRACKED agent dispatch beating for the whole run (a no-op
    // for the window-less outbox path); the guard aborts the pump on every exit.
    let _liveness = spawn_liveness_pump(writer.clone(), &request);
    let spec = AgentRunSpec::new(
        request.workflow_id.clone(),
        activity_id,
        attempt,
        // The dispatched activity-type name is neutral run identity the harness may
        // use (e.g. to label the run); it is threaded verbatim from the request.
        request.activity_type.clone(),
        Payload::new(ContentType::Json, request.input.clone()),
    );
    // Stream the transcript LIVE while the agent runs, through the shared slot so
    // it re-resolves the survivor after a redial rather than a dead socket (#254).
    let (event_sender, drain) = spawn_event_drain(drain);
    let outcome = spawn_dyn_agent(harness.as_ref(), spec, event_sender, Some(control_rx)).await;
    drain.finish().await;
    // A harness fault is a retryable activity failure, mapped as the gRPC driver does.
    let outcome =
        outcome.unwrap_or_else(|error| crate::runtime::agent::harness_error_to_outcome(&error));
    let response = DispatchResponse {
        workflow_id: request.workflow_id.clone(),
        ordinal: request.ordinal,
        run_id: request.run_id.clone(),
        outcome: wire_outcome(outcome),
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

/// Starts the automatic liveness pump for one liveness-TRACKED dispatch, or
/// `None` when the request carries no heartbeat window (the outbox path, or a
/// pre-window server) — a no-op, byte-identical to the pre-pump behaviour.
///
/// The RUNTIME owns liveness on this transport exactly as it does on gRPC
/// (#176): the pump publishes a [`WorkerLivenessBeat`] for the dispatch on
/// [`WORKER_LIVENESS_CHANNEL`] every quarter of the server-assigned window
/// ([`liveness_pump_interval`], the same cadence the gRPC loop pumps), so a
/// healthy worker running a legitimately long activity is never expired by the
/// server's heartbeat sweeper, while a wedged process stops pumping and
/// correctly is. Dropping the returned guard aborts the pump, so it lives
/// exactly as long as the dispatch on every exit path.
fn spawn_liveness_pump(writer: PushWriter, request: &DispatchRequest) -> Option<LivenessPump> {
    if request.heartbeat_window_ms == 0 {
        return None;
    }
    let period = crate::runtime::loop_::liveness_pump_interval(Duration::from_millis(
        request.heartbeat_window_ms,
    ));
    let beat = WorkerLivenessBeat {
        workflow_id: request.workflow_id.clone(),
        ordinal: request.ordinal,
    };
    let payload = match serde_json::to_vec(&beat) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(%error, "liveness pump: failed to encode WorkerLivenessBeat");
            return None;
        }
    };
    let handle = tokio::spawn(async move {
        let mut ticks = tokio::time::interval(period);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            // A publish fault means the connection is gone; the server's
            // disconnect path owns the dispatch from here, so stop pumping.
            if let Err(error) = writer.publish(WORKER_LIVENESS_CHANNEL, payload.clone()) {
                tracing::warn!(%error, "liveness pump: failed to publish beat; stopping");
                return;
            }
        }
    });
    Some(LivenessPump { handle })
}

/// A running liveness pump tied to one dispatch; dropping it aborts the pump.
struct LivenessPump {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for LivenessPump {
    fn drop(&mut self) {
        self.handle.abort();
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

/// Maps one executed dispatch outcome onto the wire `outcome`, encoding a
/// failure with the SAME kind-prefixed reason vocabulary the engine seam parses
/// (`retryable:` / `terminal:`) — the liminal mirror of the classification the
/// gRPC transport carries as the typed `ActivityError.kind`, and byte-identical
/// to what the server's gRPC completion path hands the shared delivery callback.
/// Without the prefix, retryability is silently dropped at this wire boundary
/// and every remote failure re-enters aion unclassified.
fn wire_outcome(outcome: DispatchOutcome) -> Result<String, String> {
    match outcome {
        DispatchOutcome::Completed { output } => Ok(result_string(&output)),
        DispatchOutcome::Failed { failure } => {
            let prefix = if failure.is_retryable() {
                "retryable"
            } else {
                "terminal"
            };
            Err(format!("{prefix}:{}", failure.message))
        }
    }
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
    /// the contract that keeps it byte-compatible with the server's struct —
    /// INCLUDING the engine-parity `attempt`/`labels` and the liveness
    /// `heartbeat_window_ms` the bridge stamps.
    #[test]
    fn dispatch_request_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let request = DispatchRequest {
            activity_type: "charge-card".to_owned(),
            workflow_id: WorkflowId::new(Uuid::new_v4()),
            ordinal: 7,
            run_id: Some(RunId::new(Uuid::new_v4())),
            input: br#"{"amount":42}"#.to_vec(),
            attempt: 3,
            labels: std::collections::BTreeMap::from([("region".to_owned(), "apac".to_owned())]),
            heartbeat_window_ms: 30_000,
        };
        let bytes = serde_json::to_vec(&request)?;
        let decoded: DispatchRequest = serde_json::from_slice(&bytes)?;
        assert_eq!(decoded, request);
        // The field names the server depends on are present in the JSON.
        let json = String::from_utf8(bytes)?;
        for field in [
            "activity_type",
            "workflow_id",
            "ordinal",
            "run_id",
            "input",
            "attempt",
            "labels",
            "heartbeat_window_ms",
        ] {
            assert!(json.contains(field), "wire JSON must carry `{field}`");
        }
        Ok(())
    }

    /// A frame from a server that predates `attempt`/`labels`/
    /// `heartbeat_window_ms` still decodes — as a first delivery with no labels
    /// and no liveness window (no pump) — so the wire change is
    /// backward-compatible in both directions.
    #[test]
    fn dispatch_request_without_new_fields_decodes_as_first_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        let old_frame = serde_json::json!({
            "activity_type": "charge-card",
            "workflow_id": WorkflowId::new(Uuid::new_v4()),
            "ordinal": 7,
            "run_id": null,
            "input": [123, 125],
        });
        let decoded: DispatchRequest = serde_json::from_value(old_frame)?;
        assert_eq!(
            decoded.attempt, 1,
            "a pre-attempt frame is a first delivery"
        );
        assert!(
            decoded.labels.is_empty(),
            "a pre-labels frame has no labels"
        );
        assert_eq!(
            decoded.heartbeat_window_ms, 0,
            "a pre-window frame arms no liveness pump"
        );
        Ok(())
    }

    /// The liveness beat round-trips with stable field names — the cross-crate
    /// contract with the server's mirrored `WorkerLivenessBeat`.
    #[test]
    fn worker_liveness_beat_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let beat = super::WorkerLivenessBeat {
            workflow_id: WorkflowId::new(Uuid::new_v4()),
            ordinal: 9,
        };
        let bytes = serde_json::to_vec(&beat)?;
        let decoded: super::WorkerLivenessBeat = serde_json::from_slice(&bytes)?;
        assert_eq!(decoded, beat);
        let json = String::from_utf8(bytes)?;
        for field in ["workflow_id", "ordinal"] {
            assert!(json.contains(field), "beat JSON must carry `{field}`");
        }
        // The channel name is the pinned cross-crate contract.
        assert_eq!(super::WORKER_LIVENESS_CHANNEL, "aion.worker.liveness");
        Ok(())
    }

    /// The capabilities announcement round-trips with stable field names — the
    /// cross-crate contract with the server's mirrored
    /// `WorkerCapabilitiesAnnouncement` — and the channel name is pinned.
    #[test]
    fn worker_capabilities_announcement_round_trips_through_json()
    -> Result<(), Box<dyn std::error::Error>> {
        let announcement = super::WorkerCapabilitiesAnnouncement {
            capabilities: aion_core::InterventionCapabilities {
                supported: vec![
                    aion_core::InterventionPrimitive::InjectMessage,
                    aion_core::InterventionPrimitive::Cancel,
                ],
            },
        };
        let bytes = serde_json::to_vec(&announcement)?;
        let decoded: super::WorkerCapabilitiesAnnouncement = serde_json::from_slice(&bytes)?;
        assert_eq!(decoded, announcement);
        let json = String::from_utf8(bytes)?;
        for field in ["capabilities", "supported"] {
            assert!(json.contains(field), "wire JSON must carry `{field}`");
        }
        assert_eq!(
            super::WORKER_CAPABILITIES_CHANNEL,
            "aion.worker.capabilities"
        );
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
            attempt: 1,
            labels: std::collections::BTreeMap::new(),
            heartbeat_window_ms: 0,
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
