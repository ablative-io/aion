//! Cross-node outbox dispatch over the liminal bus (LSUB push transport).
//!
//! # What this is (production push path)
//!
//! This module wires the durable outbox's fan-out dispatch over liminal to a
//! REAL remote aion worker and returns the worker's result through the existing
//! [`OutboxDeliveryCallback`](super::bridge::OutboxDeliveryCallback), behind the
//! `liminal-transport` Cargo feature and the `outbox.transport = liminal`
//! runtime flag. The aion-server HOSTS the liminal listener: a remote worker
//! connects IN and self-describes in-band, the server registers it in the SAME
//! connected-worker registry a gRPC worker joins, and a claimed row is PUSHED out
//! on the worker's existing connection (the LSUB-0 server-push primitive).
//!
//! # Routing (NSTQ-5 / NODE-5)
//!
//! A worker is selected by the row's `(namespace, task_queue, activity_type,
//! node)` pool key through the EXISTING registry `select_worker` — the same
//! selection the gRPC path uses, so routing semantics are shared. `activity_type`
//! is NOT a routing dimension at the wire: it rides inside the [`DispatchRequest`]
//! payload and is matched by the worker after delivery, exactly as the gRPC
//! registry pushes `activity_type` in the task body while selecting the worker by
//! pool key. See `docs/NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md` §4.2. The
//! [`dispatch_channel_name`] derivation remains the single source of truth for the
//! pool-channel string, pinned for any future channel-subscription subscriber so
//! the two sides cannot drift.
//!
//! # The seams it implements
//!
//! - [`RegistryLiminalDispatch`] implements
//!   [`OutboxRowDispatch`](super::outbox_dispatcher::OutboxRowDispatch): for each
//!   claimed row it selects a worker from the connected-worker registry, pushes
//!   the [`DispatchRequest`] to that worker's liminal connection via its
//!   [`LiminalWorkerDelivery`], and re-enters the worker's [`DispatchResponse`]
//!   through the SAME [`LiminalCompletionSource`] / [`OutboxDeliveryCallback`] the
//!   gRPC completion path uses. A row that reaches no matching worker, or whose
//!   worker is not liminal-delivered, returns an error so the outbox's unchanged
//!   retry/backoff drives it — the same honest no-worker contract as the gRPC
//!   path.
//! - [`LiminalConnectionNotifier`] is the SERVER half of in-band registration:
//!   when a worker connects with a [`WorkerRegistration`](WireWorkerRegistration)
//!   the notifier inserts a [`WorkerDelivery::Liminal`] into the registry, and
//!   drops it on disconnect.
//! - [`LiminalCompletionSource`] maps a [`DispatchResponse`] onto the delivery
//!   callback, threading `run_id` end-to-end so the existing continue-as-new run
//!   gates apply unchanged.
//!
//! # The channel-subscription seam (documented, distinct from the push path)
//!
//! [`dispatch_channel_name`] derives the pool channel a `(namespace, task_queue)`
//! pool addresses, optionally pinned to a `node` (NODE-2/NODE-5). The production
//! path above does not publish to that channel — it pushes to a connected worker
//! the server already owns — but the derivation is retained as the pinned contract
//! any future channel-subscription transport MUST honour so the dispatcher and a
//! subscriber cannot drift:
//!
//! - An UNPINNED worker pool addressed `(namespace, task_queue)` subscribes to
//!   `dispatch_channel_name(namespace, task_queue, None)`.
//! - A NODE-PINNED dispatch (the row carries `Some(node)`) maps to
//!   `dispatch_channel_name(namespace, task_queue, Some(node))` — a DISTINCT
//!   channel that a worker on that node must ALSO subscribe to in order to serve
//!   pinned work; the unpinned channel alone never delivers a pinned dispatch.
//!
//! That is the single contract the seam must honour.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aion_core::{ActivityId, ContentType, Payload, RunId, WorkflowId};
use aion_store::OutboxRow;
use async_trait::async_trait;
use liminal::protocol::WorkerRegistration as WireWorkerRegistration;
use liminal_sdk::{SchemaMetadata, SchemaValidate};
use liminal_server::ServerError as LiminalServerError;
use liminal_server::server::connection::{
    ConnectionNotifier, ConnectionSupervisor, PushReplyAwaiter,
};
use serde::{Deserialize, Serialize};

use super::bridge::OutboxDeliveryCallback;
use super::outbox_dispatcher::OutboxRowDispatch;
use super::registry::{ConnectedWorkerRegistry, WorkerDelivery, WorkerHandle, WorkerRegistration};
use crate::error::ServerError;

/// Upper bound on how long a server-initiated dispatch push waits for the
/// worker's correlated reply before the row is treated as undelivered and the
/// outbox retries. Generous because an activity may legitimately run a while; the
/// outbox's own retry/reconcile loop is the real liveness backstop.
const PUSH_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Re-arm cadence for the engine-seam bridge's UNBOUNDED reply wait
/// ([`receive_bridge_reply`]). Each elapsed poll is a benign re-arm, never a
/// failure: the bridge dispatch contract imposes no activity timeout of its own
/// (agent-style activities legitimately run for over an hour), exactly like the
/// gRPC bridge's unbounded `recv`. Worker loss still terminates the wait
/// promptly — the awaiter wakes with the typed Disconnected error the moment the
/// connection closes.
const BRIDGE_REPLY_POLL: Duration = Duration::from_secs(1);

/// Wire request carrying one scheduled activity to a liminal worker.
///
/// Mirrors the dispatch half of the gRPC `ActivityTask`: the fields the worker
/// needs to execute the activity and to correlate its result back to the exact
/// execution (`workflow_id`, `ordinal`, `run_id`). `run_id` rides end-to-end so
/// the existing continue-as-new run gates hold over the liminal wire (the design
/// doc §3.3 requirement that `RunId` stays on the wire).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispatchRequest {
    /// Activity type the worker must execute.
    pub activity_type: String,
    /// Workflow that scheduled this fan-out activity. Carried in its serde form
    /// so no fragile id parsing happens on the wire.
    pub workflow_id: WorkflowId,
    /// Pinned ordinal of this activity within the workflow's fan-out range.
    pub ordinal: u64,
    /// Run that dispatched this ordinal, when known (continue-as-new safety).
    pub run_id: Option<RunId>,
    /// Opaque activity input bytes (JSON-tagged on the aion side).
    pub input: Vec<u8>,
}

impl SchemaValidate for DispatchRequest {
    fn schema_metadata() -> SchemaMetadata {
        SchemaMetadata::new(
            "aion.outbox.dispatch.request",
            "1",
            br#"{"type":"object"}"#.as_slice(),
        )
    }
}

/// Wire response carrying one worker result back to the outbox.
///
/// Mirrors the completion half of the gRPC `ActivityResult`: the correlation ids
/// plus either a success result or a failure reason. `LiminalCompletionSource`
/// maps this onto the existing [`OutboxDeliveryCallback`].
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

impl SchemaValidate for DispatchResponse {
    fn schema_metadata() -> SchemaMetadata {
        SchemaMetadata::new(
            "aion.outbox.dispatch.response",
            "1",
            br#"{"type":"object"}"#.as_slice(),
        )
    }
}

/// Wire request carrying one neutral mid-run intervention command to a liminal
/// worker (NOI-6, §6.2).
///
/// Rides the SAME liminal server-push channel as [`DispatchRequest`], distinguished
/// on the wire by its unique required `intervention` field — a plain
/// [`DispatchRequest`] has no such field, so the worker demuxes the two by which
/// one deserializes. The whole envelope is neutral: it carries an
/// [`InterventionCommand`], never a harness type. Field-for-field mirrored by the
/// worker's `liminal::InterventionRequest`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterventionRequest {
    /// The neutral command to route to the worker owning the target attempt.
    pub intervention: aion_core::InterventionCommand,
}

impl SchemaValidate for InterventionRequest {
    fn schema_metadata() -> SchemaMetadata {
        SchemaMetadata::new(
            "aion.intervention.request",
            "1",
            br#"{"type":"object"}"#.as_slice(),
        )
    }
}

/// Wire response carrying the worker's neutral intervention ack back to the server
/// (NOI-6). Field-for-field mirrored by the worker's `liminal::InterventionReply`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterventionReply {
    /// The neutral applied/gated/stale outcome the operator receives.
    pub outcome: aion_core::InterventionOutcome,
}

impl SchemaValidate for InterventionReply {
    fn schema_metadata() -> SchemaMetadata {
        SchemaMetadata::new(
            "aion.intervention.reply",
            "1",
            br#"{"type":"object"}"#.as_slice(),
        )
    }
}

/// Builds the wire request for one claimed outbox row.
///
/// Kept free-standing (not a method) so both the dispatch path and tests build
/// the request the same way.
#[must_use]
pub fn request_for_row(row: &OutboxRow) -> DispatchRequest {
    DispatchRequest {
        activity_type: row.activity_type.clone(),
        workflow_id: row.workflow_id.clone(),
        ordinal: row.ordinal,
        run_id: row.run_id.clone(),
        input: row.input.bytes().to_vec(),
    }
}

/// The single reserved character that separates channel segments. Because
/// `namespace`/`task_queue` are free-form, any occurrence of this byte INSIDE a
/// segment must be escaped so it cannot be mistaken for the segment boundary.
const SEGMENT_SEPARATOR: char = '.';

/// The escape character used by [`encode_segment`]. It must itself be escaped so
/// the encoding stays injective (otherwise `%2E` as a literal field value would
/// collide with an encoded `.`).
const SEGMENT_ESCAPE: char = '%';

/// Percent-encodes the two reserved characters (`.` and `%`) inside one channel
/// segment so distinct segment values can never collide across the join.
///
/// This is a minimal, deterministic, per-segment escape: a literal `.` becomes
/// `%2E` and a literal `%` becomes `%25`; every other byte (including the empty
/// string) passes through unchanged. Because both the separator AND the escape
/// char are encoded, the mapping `value -> encoded` is injective: it is exactly
/// reversible by replacing `%2E -> .` and `%25 -> %`, so two distinct values
/// can never encode to the same string. Dot-free, percent-free inputs (the
/// normal case, e.g. `"remote"`, `"gpu"`) are returned byte-for-byte unchanged,
/// so existing channels are stable.
fn encode_segment(segment: &str) -> String {
    // Fast path: nothing reserved, return an owned copy unchanged.
    if !segment.contains([SEGMENT_SEPARATOR, SEGMENT_ESCAPE]) {
        return segment.to_owned();
    }
    let mut encoded = String::with_capacity(segment.len());
    for ch in segment.chars() {
        match ch {
            // Encode the escape char FIRST so an already-present `%` cannot be
            // confused with one we introduce for the separator.
            SEGMENT_ESCAPE => encoded.push_str("%25"),
            SEGMENT_SEPARATOR => encoded.push_str("%2E"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// Derives the liminal dispatch channel for a worker pool addressed
/// `(namespace, task_queue)`, optionally pinned to a specific `node`.
///
/// This is the **single, total source of truth** for the channel string: every
/// site that needs the channel a `(namespace, task_queue[, node])` pool
/// dispatches to — both this dispatcher and any future worker-pool subscription
/// side — MUST call this function so the two sides cannot drift. The format is
/// `"aion.dispatch.{namespace}.{task_queue}"` for an unpinned dispatch and
/// `"aion.dispatch.{namespace}.{task_queue}.{node}"` when a `node` is pinned;
/// each `{segment}` is independently passed through [`encode_segment`].
///
/// # The subscriber contract (the seam this function pins, NODE-5 / 13-x)
///
/// The subscriber side remains the documented seam (it does not exist yet; 13-0
/// uses liminal's in-server echo responder). The contract both sides MUST honour:
///
/// - An **unpinned** worker pool addressed `(namespace, task_queue)` subscribes
///   to `dispatch_channel_name(namespace, task_queue, None)` and receives every
///   unpinned dispatch for that pool.
/// - A **node-pinned** dispatch (the row carries `Some(node)`) goes to
///   `dispatch_channel_name(namespace, task_queue, Some(node))`, a DISTINCT
///   channel. A worker running on that node which is meant to serve pinned work
///   for the pool MUST ALSO subscribe to that node-specific channel — the
///   `None` channel alone will never deliver a node-pinned dispatch to it.
///
/// Because the `None` channel and any `Some(node)` channel are distinct strings,
/// a node-pinned dispatch never reaches an unpinned-only subscriber and vice
/// versa; node isolation is therefore enforced by the channel string itself.
///
/// # Injectivity (why the per-segment encode matters)
///
/// `namespace`, `task_queue` and `node` are all free-form (the design forbids
/// preset categories), so a raw `format!` would be NON-injective: a `.` inside
/// any field bleeds across the separator and pools the design declares disjoint
/// collide onto one channel — e.g. `("a.b", "c", None)` and `("a", "b.c", None)`
/// would both yield `aion.dispatch.a.b.c`, a cross-pool leak on the very
/// isolation dimension this routing exists to keep separate. Encoding each
/// segment independently (the separator `.` and the escape `%` are escaped within
/// a segment) makes the map from `(namespace, task_queue, node)` to channel
/// string injective: distinct triples always yield distinct channels, ACROSS
/// segment counts too. The node segment is appended only for `Some(node)`, and
/// because no encoded segment can contain a bare separator, a 2-segment channel
/// (unpinned) can never be confused with a 3-segment channel (pinned) — e.g.
/// `("a", "b", Some("c"))` and `("a", "b.c", None)` stay distinct, as do
/// `("a.b", "c", None)` and `("a", "b", Some("c"))`.
///
/// `activity_type` is deliberately NOT part of the channel: it is *what to run*,
/// matched by the worker after delivery (it rides inside [`DispatchRequest`]),
/// not *which pool* — see `docs/NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md` §4.2. The
/// function is total (defined for every input) and stable (the same
/// `(namespace, task_queue, node)` always yields the same channel).
#[must_use]
pub fn dispatch_channel_name(namespace: &str, task_queue: &str, node: Option<&str>) -> String {
    let namespace = encode_segment(namespace);
    let task_queue = encode_segment(task_queue);
    match node {
        Some(node) => {
            let node = encode_segment(node);
            format!("aion.dispatch.{namespace}.{task_queue}.{node}")
        }
        None => format!("aion.dispatch.{namespace}.{task_queue}"),
    }
}

/// Derives the liminal dispatch channel for a claimed outbox row.
///
/// Thin wrapper over [`dispatch_channel_name`] reading the row's durable
/// `(namespace, task_queue)` (NSTQ-2 columns) and its optional `node` (NODE-2):
/// when `row.node` is `Some`, the row dispatches to the node-pinned sub-channel;
/// when `None`, it derives the byte-identical unpinned channel. Kept
/// free-standing so the dispatch path and tests derive the row's channel
/// identically.
#[must_use]
pub fn channel_for_row(row: &OutboxRow) -> String {
    dispatch_channel_name(&row.namespace, &row.task_queue, row.node.as_deref())
}

/// Wraps a reason in the existing worker-dispatch error so a non-deliverable
/// dispatch drives the outbox's unchanged retry/backoff/dead-letter path. The
/// row-derived `channel` is surfaced as the `activity_type` field for operator
/// diagnostics (the field is a free-form context string on this transport's
/// error).
fn dispatch_error(channel: &str, reason: String) -> ServerError {
    ServerError::WorkerDispatch {
        namespace: "liminal".to_owned(),
        activity_type: channel.to_owned(),
        reason,
    }
}

/// Receives one worker result over liminal and re-enters it into aion.
///
/// Holds the installed [`OutboxDeliveryCallback`] (the same prod
/// `ServerOutboxDeliveryCallback` the gRPC completion path uses) and maps a
/// [`DispatchResponse`] onto it, threading `run_id` so the continue-as-new run
/// gates apply unchanged.
pub struct LiminalCompletionSource {
    callback: Arc<dyn OutboxDeliveryCallback>,
}

impl std::fmt::Debug for LiminalCompletionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiminalCompletionSource")
            .finish_non_exhaustive()
    }
}

impl LiminalCompletionSource {
    /// Build a completion source over the shared outbox delivery callback.
    #[must_use]
    pub fn new(callback: Arc<dyn OutboxDeliveryCallback>) -> Self {
        Self { callback }
    }

    /// Re-enter one worker result into aion through the delivery callback.
    ///
    /// Returns the callback's `bool`: `true` when delivered to a live run,
    /// `false` when no run is live (the expected stale-completion drop that
    /// recovery re-arms). A success outcome routes to `deliver_completion`; a
    /// failure outcome to `deliver_failure`.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the response carries an unparseable id or
    /// the engine rejects the delivery.
    pub fn deliver(&self, response: &DispatchResponse) -> Result<bool, ServerError> {
        let activity_id = ActivityId::from_sequence_position(response.ordinal);
        match &response.outcome {
            Ok(result) => self.callback.deliver_completion(
                &response.workflow_id,
                &activity_id,
                response.run_id.as_ref(),
                result.clone(),
            ),
            Err(reason) => self.callback.deliver_failure(
                &response.workflow_id,
                &activity_id,
                response.run_id.as_ref(),
                reason.clone(),
            ),
        }
    }
}

/// Rebuilds the activity input payload from the wire request.
///
/// The aion side tags activity input as JSON; the wire carries the raw bytes, so
/// a worker (or the test responder standing in for one) reconstructs the typed
/// [`Payload`] with the JSON content type.
#[must_use]
pub fn payload_from_request(request: &DispatchRequest) -> Payload {
    Payload::new(ContentType::Json, request.input.clone())
}

/// Delivery handle for a liminal-connected worker held in the worker registry.
///
/// A worker that connects over liminal is a first-class registry member selected
/// the SAME way as a gRPC worker (`select_worker` on `(namespace, task_queue,
/// node)`); this is the delivery leg the registry holds for it. It pairs the
/// [`ConnectionSupervisor`] that owns the worker's connection with that
/// connection's beamr `pid`, so [`Self::dispatch`] can push a [`DispatchRequest`]
/// out on the worker's existing socket (the LSUB-0 server-push primitive) and
/// block for the correlated [`DispatchResponse`].
#[derive(Clone)]
pub struct LiminalWorkerDelivery {
    supervisor: ConnectionSupervisor,
    pid: u64,
}

impl std::fmt::Debug for LiminalWorkerDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiminalWorkerDelivery")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl LiminalWorkerDelivery {
    /// Build a delivery handle for the worker reachable on connection `pid`
    /// through `supervisor`.
    #[must_use]
    pub const fn new(supervisor: ConnectionSupervisor, pid: u64) -> Self {
        Self { supervisor, pid }
    }

    /// The connection pid this worker is addressed on.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.pid
    }

    /// Push one dispatch out on the worker's connection and block for its reply.
    ///
    /// Serializes `request`, pushes it via [`ConnectionSupervisor::push_to_connection`],
    /// and decodes the worker's correlated [`DispatchResponse`] reply.
    ///
    /// # Error classification (LSUB-3)
    ///
    /// Two of the failure paths mean the chosen worker's connection is GONE, and
    /// they surface the typed [`ServerError::WorkerConnectionLost`] so the outbox
    /// can fail over immediately rather than waiting out the retry backoff:
    ///
    /// - `push_to_connection` returns `Err` only when the connection process is no
    ///   longer live (the connection was already gone at push time). It has no
    ///   other failure mode, so that whole arm is connection-lost.
    /// - `awaiter.receive` returns the typed liminal
    ///   `ServerError::PushReplyDisconnected` when the connection closed before a
    ///   correlated reply arrived (after Stage A this wakes PROMPTLY instead of
    ///   blocking the full [`PUSH_REPLY_TIMEOUT`]), and `PushReplyTimeout` when the
    ///   worker is alive but slow. [`is_connection_closed_reply_error`] matches the
    ///   Disconnected variant BY TYPE; the Timeout variant (and anything else)
    ///   stays on the existing [`ServerError::WorkerDispatch`] backoff path — the
    ///   two are never collapsed.
    ///
    /// Every other failure (serialize, decode, or any unrecognized reply error)
    /// remains a [`ServerError::WorkerDispatch`] so the outbox's unchanged
    /// backoff/dead-letter path drives the row.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::WorkerConnectionLost`] when the worker connection was
    /// gone at push time or closed before replying; returns
    /// [`ServerError::WorkerDispatch`] when the request cannot be serialized, the
    /// reply does not arrive within [`PUSH_REPLY_TIMEOUT`] (slow worker), or the
    /// reply cannot be decoded.
    pub fn dispatch(&self, request: &DispatchRequest) -> Result<DispatchResponse, ServerError> {
        let awaiter = self.push_dispatch(request)?;
        let reply = awaiter.receive(PUSH_REPLY_TIMEOUT).map_err(|error| {
            // Disconnected (worker died mid-flight) => connection-lost => immediate
            // failover. Timeout (worker alive but slow) and anything unrecognized
            // => WorkerDispatch => unchanged backoff. Never collapse the two.
            if is_connection_closed_reply_error(&error) {
                ServerError::worker_connection_lost(
                    "liminal-push",
                    format!("worker connection closed before reply: {error}"),
                )
            } else {
                dispatch_error("liminal-push", format!("worker reply failed: {error}"))
            }
        })?;
        decode_dispatch_response(&reply)
    }

    /// Serialize and push one dispatch out on the worker's connection, returning
    /// the awaiter for its correlated reply — the shared push half of both
    /// dispatch waits: [`Self::dispatch`] (the outbox path, bounded by
    /// [`PUSH_REPLY_TIMEOUT`] because the outbox's retry loop re-drives an
    /// undelivered row) and the engine-seam bridge dispatcher (which owns the
    /// UNBOUNDED wait of [`receive_bridge_reply`], matching the gRPC bridge
    /// contract). One frame format, one push primitive, two wait policies.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::WorkerConnectionLost`] when the connection process
    /// is no longer live at push time (a push-enqueue failure has exactly one
    /// cause in liminal — the worker is already gone), and
    /// [`ServerError::WorkerDispatch`] when the request cannot be serialized.
    pub(crate) fn push_dispatch(
        &self,
        request: &DispatchRequest,
    ) -> Result<PushReplyAwaiter, ServerError> {
        let payload = serde_json::to_vec(request).map_err(|error| {
            dispatch_error("liminal-push", format!("request serialize failed: {error}"))
        })?;
        self.supervisor
            .push_to_connection(self.pid, payload)
            .map_err(|error| {
                ServerError::worker_connection_lost(
                    "liminal-push",
                    format!("push to worker failed: {error}"),
                )
            })
    }

    /// Push one neutral intervention command out on the worker's connection and
    /// block for its correlated ack reply (NOI-6, §6.2).
    ///
    /// Mirrors [`Self::dispatch`] but carries an [`InterventionRequest`] and decodes
    /// an [`InterventionReply`], so an intervention rides the SAME server-push
    /// channel as an activity dispatch. The push is a blocking, thread-based liminal
    /// call; the async router runs it off the runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::WorkerConnectionLost`] when the worker connection was
    /// gone at push time or closed before replying (so the router surfaces the
    /// too-late no-op); returns [`ServerError::WorkerDispatch`] when the request
    /// cannot be serialized, the reply times out, or the reply cannot be decoded.
    pub fn push_intervention(
        &self,
        request: &InterventionRequest,
    ) -> Result<InterventionReply, ServerError> {
        let payload = serde_json::to_vec(request).map_err(|error| {
            dispatch_error(
                "liminal-push",
                format!("intervention serialize failed: {error}"),
            )
        })?;
        let awaiter = self
            .supervisor
            .push_to_connection(self.pid, payload)
            .map_err(|error| {
                ServerError::worker_connection_lost(
                    "liminal-push",
                    format!("push intervention to worker failed: {error}"),
                )
            })?;
        let reply = awaiter.receive(PUSH_REPLY_TIMEOUT).map_err(|error| {
            if is_connection_closed_reply_error(&error) {
                ServerError::worker_connection_lost(
                    "liminal-push",
                    format!("worker connection closed before intervention ack: {error}"),
                )
            } else {
                dispatch_error("liminal-push", format!("intervention ack failed: {error}"))
            }
        })?;
        serde_json::from_slice(&reply).map_err(|error| {
            dispatch_error(
                "liminal-push",
                format!("intervention ack decode failed: {error}"),
            )
        })
    }
}

/// Decodes one correlated reply payload as a [`DispatchResponse`].
///
/// Shared by the outbox wait ([`LiminalWorkerDelivery::dispatch`]) and the
/// bridge wait ([`receive_bridge_reply`]) so the two paths can never diverge on
/// the wire's reply shape.
fn decode_dispatch_response(reply: &[u8]) -> Result<DispatchResponse, ServerError> {
    serde_json::from_slice(reply).map_err(|error| {
        dispatch_error(
            "liminal-push",
            format!("worker reply decode failed: {error}"),
        )
    })
}

/// Blocks for the correlated reply to an engine-seam BRIDGE dispatch push, with
/// the bridge's UNBOUNDED wait contract: the engine imposes no activity timeout
/// of its own, so an elapsed [`BRIDGE_REPLY_POLL`] merely re-arms the wait — the
/// exact liminal mirror of the gRPC bridge's unbounded `recv`, which is released
/// only by a completion or by stream teardown. The wait terminates on exactly:
///
/// - **the reply** — decoded as the worker's [`DispatchResponse`];
/// - **worker loss** — the connection closed before replying: the awaiter wakes
///   PROMPTLY with liminal's typed Disconnected error, surfaced as
///   [`ServerError::WorkerConnectionLost`] so the bridge reports the same
///   retryable lost-worker failure the gRPC teardown sweep does;
/// - **an unrecognized receive fault or a decode failure** — surfaced as
///   [`ServerError::WorkerDispatch`].
///
/// Runs on a dedicated bridge reply thread, never on an async runtime worker.
///
/// # Errors
///
/// Returns [`ServerError::WorkerConnectionLost`] on worker loss and
/// [`ServerError::WorkerDispatch`] on a receive fault or reply decode failure.
pub(crate) fn receive_bridge_reply(
    awaiter: &PushReplyAwaiter,
) -> Result<DispatchResponse, ServerError> {
    loop {
        match awaiter.receive(BRIDGE_REPLY_POLL) {
            Ok(reply) => return decode_dispatch_response(&reply),
            // A bare poll timeout is a re-arm, never a failure: the dispatch
            // wait is unbounded by contract (see the bridge module docs).
            Err(LiminalServerError::PushReplyTimeout { .. }) => {}
            Err(error) if is_connection_closed_reply_error(&error) => {
                return Err(ServerError::worker_connection_lost(
                    "liminal-push",
                    format!("worker connection closed before reply: {error}"),
                ));
            }
            Err(error) => {
                return Err(dispatch_error(
                    "liminal-push",
                    format!("worker reply failed: {error}"),
                ));
            }
        }
    }
}

/// Returns true when a liminal push-reply error is the *Disconnected* case (the
/// worker's connection closed before it replied), as opposed to a genuine reply
/// timeout (the worker is alive but slow).
///
/// Liminal returns these as distinct TYPED variants —
/// `ServerError::PushReplyDisconnected` vs `PushReplyTimeout` (see `liminal-server`
/// `supervisor.rs` `PushReplyAwaiter::receive`) — so this is a type match, not a
/// message-text match: a worker that DIED (connection-lost, fast failover) is told
/// apart from one that is merely SLOW (genuine timeout, normal backoff) by variant.
fn is_connection_closed_reply_error(error: &LiminalServerError) -> bool {
    matches!(error, LiminalServerError::PushReplyDisconnected { .. })
}

/// Cross-node [`OutboxRowDispatch`] that selects a liminal worker from the
/// connected-worker registry and pushes the row to it.
///
/// This is the LSUB-1 server-side composition: for each claimed row it selects a
/// worker by the row's `(namespace, task_queue, activity_type, node)` via the
/// EXISTING registry `select_worker` (the same selection the gRPC path uses, so
/// routing semantics are shared), pushes the [`DispatchRequest`] to that worker's
/// liminal connection via its [`LiminalWorkerDelivery`], and re-enters the
/// worker's [`DispatchResponse`] through the SAME [`LiminalCompletionSource`] /
/// [`OutboxDeliveryCallback`] the existing completion path uses. A row that
/// reaches no matching worker, or whose worker is not liminal-delivered, returns
/// an error so the outbox's unchanged retry/backoff drives it — the same honest
/// no-worker contract as the gRPC path.
pub struct RegistryLiminalDispatch {
    registry: ConnectedWorkerRegistry,
    completion: LiminalCompletionSource,
    /// Optional short-TTL per-namespace placement cache (Control-Plane Phase 2,
    /// P2-P3), the SAME cache the gRPC
    /// [`WorkerOutboxDispatch`](crate::worker::WorkerOutboxDispatch) is given. When
    /// present, an UNPINNED row (`row.node == None`) whose namespace placement is
    /// `Prefer{L}` selects an L-labelled worker and spills to any live worker when
    /// none is up, via the SHARED
    /// [`preferred_node_order`](crate::worker::preferred_node_order). When absent
    /// (the default, every pre-Phase-2 construction and test) selection is
    /// byte-identical to before: one `select_worker` off the row's own node.
    /// Placement is NEVER stamped back onto the row — it is consulted only here,
    /// in this non-replayed dispatcher, for worker selection.
    placement_cache: Option<crate::worker::PlacementCache>,
    /// Optional NOI-6 `attempt -> owning-worker` back-index. When installed via
    /// [`Self::with_attempt_owners`], each dispatched agent attempt binds its
    /// `(workflow, activity, attempt)` to the selected worker here BEFORE the push and
    /// releases it after the reply, so the server's intervention router resolves the
    /// CURRENT owner of a live attempt. `None` (the default, and every non-agent
    /// deployment) skips the binding — intervention is simply never offered.
    attempt_owners: Option<super::intervention::AttemptOwnerIndex>,
}

impl std::fmt::Debug for RegistryLiminalDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryLiminalDispatch")
            .field("placement_cache", &self.placement_cache.is_some())
            .finish_non_exhaustive()
    }
}

impl RegistryLiminalDispatch {
    /// Build a registry-backed liminal dispatch that re-enters worker results
    /// through `callback` (the shared `ServerOutboxDeliveryCallback`).
    #[must_use]
    pub fn new(
        registry: ConnectedWorkerRegistry,
        callback: Arc<dyn OutboxDeliveryCallback>,
    ) -> Self {
        Self {
            registry,
            completion: LiminalCompletionSource::new(callback),
            placement_cache: None,
            attempt_owners: None,
        }
    }

    /// Install the NOI-6 attempt-owner back-index so each dispatched attempt binds
    /// its owning worker for the intervention router to resolve (NOI-6).
    ///
    /// The SAME index the server's [`InterventionRouter`](super::intervention::InterventionRouter)
    /// resolves through (from `ServerState::attempt_owners`), so a pushed command
    /// reaches the worker this dispatcher sent the attempt to. Pure builder addition:
    /// without it, no ownership is recorded and the router finds no owner (the
    /// too-late no-op), exactly as before.
    #[must_use]
    pub fn with_attempt_owners(
        mut self,
        attempt_owners: super::intervention::AttemptOwnerIndex,
    ) -> Self {
        self.attempt_owners = Some(attempt_owners);
        self
    }

    /// Attach the per-namespace placement cache so an unpinned row consults its
    /// namespace's `Prefer` directive at selection time (Control-Plane Phase 2,
    /// P2-P3) — the liminal mirror of
    /// [`WorkerOutboxDispatch::with_placement_cache`](crate::worker::WorkerOutboxDispatch::with_placement_cache).
    /// Pure builder addition: without it, selection is byte-identical to the
    /// pre-Phase-2 behaviour.
    #[must_use]
    pub fn with_placement_cache(mut self, cache: crate::worker::PlacementCache) -> Self {
        self.placement_cache = Some(cache);
        self
    }

    /// Select the liminal worker for `row`, applying the SHARED placement decision
    /// for an UNPINNED row when a placement cache is attached — the exact gRPC
    /// semantics ([`worker_selection_for`](crate::worker::worker_selection_for)):
    /// `Prefer{L}` spills to any live worker, `Pinned{L}` requires an L-labelled
    /// worker and NEVER spills to a node=None any-worker.
    ///
    /// A per-activity authored pin (`row.node == Some(N)`) ALWAYS wins and is
    /// selected off the row's own node, untouched by placement — exactly the gRPC
    /// composition rule. Without a cache (or with a pinned row) this collapses to
    /// the single `select_worker` off the row's own node — the pre-Phase-2
    /// behaviour.
    ///
    /// For `Pinned{L}`, when no L-labelled worker is live this returns `Ok(None)` —
    /// NOT a spill to a node=None worker — so the [`OutboxRowDispatch`] surfaces the
    /// honest no-worker error and the outbox retries/stalls until an L-labelled
    /// worker returns, mirroring the gRPC wait-for-worker path exactly (both
    /// transports agree via [`WorkerSelection`](crate::worker::WorkerSelection)).
    async fn select_liminal_worker(
        &self,
        row: &OutboxRow,
    ) -> Result<Option<WorkerHandle>, ServerError> {
        // A pinned row or an absent cache: one selection off the row's own node.
        let (Some(cache), None) = (&self.placement_cache, &row.node) else {
            return self.registry.select_worker(
                &row.namespace,
                &row.task_queue,
                &row.activity_type,
                row.node.as_deref(),
            );
        };
        // Unpinned + placement-aware: resolve the shared selection decision, so this
        // liminal path and the gRPC path can never diverge on Prefer-vs-Pinned. The
        // row's `node` is never mutated — selection is a pure dispatch-time input.
        let placement = cache.placement(&row.namespace).await;
        match crate::worker::worker_selection_for(&placement) {
            // Prefer/Unplaced: walk the prefer-then-spill tiers (the `None` spill is
            // always last), stopping at the first tier with a live worker.
            crate::worker::WorkerSelection::PreferTiers(tiers) => {
                self.select_over_tiers(row, tiers.iter().map(Option::as_deref))
            }
            // Pinned{L}: try ONLY the required labels — no `None` spill. When none is
            // live, return None so the caller retries/stalls (never any-node).
            crate::worker::WorkerSelection::Required(required) => self.select_over_tiers(
                row,
                required.iter().map(|label| Some(String::as_str(label))),
            ),
        }
    }

    /// Select the first live worker over an ordered sequence of node filters,
    /// returning `Ok(None)` when no filter matches a live worker. Shared by the
    /// `Prefer` (tiers end in a `None` spill) and `Pinned` (required labels only,
    /// no spill) selection arms so both walk the registry identically.
    fn select_over_tiers<'a>(
        &self,
        row: &OutboxRow,
        tiers: impl Iterator<Item = Option<&'a str>>,
    ) -> Result<Option<WorkerHandle>, ServerError> {
        for tier in tiers {
            let selected = self.registry.select_worker(
                &row.namespace,
                &row.task_queue,
                &row.activity_type,
                tier,
            )?;
            if selected.is_some() {
                return Ok(selected);
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl OutboxRowDispatch for RegistryLiminalDispatch {
    async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError> {
        // Select the worker the SAME way the gRPC path does: by the row's
        // (namespace, task_queue, activity_type) pool key with the row's optional
        // node affinity, applying the SHARED `Prefer` two-tier spill for an
        // unpinned row when a placement cache is attached. No worker for the pool
        // => honest no-worker error => the outbox retries (never a false `done`).
        let worker = self.select_liminal_worker(row).await?.ok_or_else(|| {
            dispatch_error(
                &channel_for_row(row),
                "no liminal worker registered for the row's pool".to_owned(),
            )
        })?;

        let delivery = match worker.delivery() {
            WorkerDelivery::Liminal(delivery) => delivery.clone(),
            WorkerDelivery::Grpc(_) => {
                return Err(dispatch_error(
                    &channel_for_row(row),
                    "selected worker is not delivered over liminal".to_owned(),
                ));
            }
        };

        // NOI-6: bind this attempt's owner BEFORE the push, so an intervention that
        // races the dispatch resolves the worker. The guard releases on every exit
        // path (reply, error, panic) so the index never keeps a finished attempt.
        // The key mirrors the worker's execute-path stamp exactly: activity_id from
        // the ordinal, attempt = 1 (the push wire carries no attempt; the worker
        // stamps a first delivery). See `LiminalActivityWorker::execute`.
        let _owner_guard = self.attempt_owners.as_ref().map(|owners| {
            let key = super::intervention::AttemptKey::new(
                row.workflow_id.clone(),
                ActivityId::from_sequence_position(row.ordinal),
                1,
            );
            owners.bind(key.clone(), worker.id());
            AttemptOwnerGuard {
                owners: owners.clone(),
                key,
            }
        });

        // Push the dispatch to the worker and block for its correlated reply. The
        // push is a blocking, thread-based liminal call; run it off the async
        // runtime so a long-running activity cannot starve a runtime worker.
        let request = request_for_row(row);
        let response = tokio::task::spawn_blocking(move || delivery.dispatch(&request))
            .await
            .map_err(|error| {
                dispatch_error(
                    &channel_for_row(row),
                    format!("dispatch task join failed: {error}"),
                )
            })??;

        // Re-enter the worker's result through the SAME completion path the gRPC
        // transport uses (terminal dedup in `record_fan_out_completion` applies
        // unchanged). The dispatch itself succeeded — the row's terminal state is
        // recorded by the completion callback, exactly as in the gRPC path.
        self.completion.deliver(&response)?;
        Ok(())
    }
}

/// RAII guard that releases an [`AttemptOwnerIndex`](super::intervention::AttemptOwnerIndex)
/// binding when the dispatch call returns — on the reply, an error, or a panic — so
/// the back-index tracks exactly the attempts currently in flight (NOI-6).
struct AttemptOwnerGuard {
    owners: super::intervention::AttemptOwnerIndex,
    key: super::intervention::AttemptKey,
}

impl Drop for AttemptOwnerGuard {
    fn drop(&mut self) {
        self.owners.release(&self.key);
    }
}

/// Normalize a wire `node` (`Option<String>`) onto the registry's optional
/// locality affinity, applying the SAME none-convention the gRPC registration
/// path uses (`registry::optional_node`): an empty string carries no node, so it
/// collapses to `None`; any non-empty value is the worker's advertised node.
///
/// The wire already models `node` as `Option<String>`, but a worker that joins
/// `Some("")` (the empty-string node) must not register a distinct empty-node
/// affinity that no pinned dispatch could ever match — it is semantically
/// unpinned, exactly as the gRPC proto3 empty default is. Folding it to `None`
/// here keeps the two registration paths byte-for-byte equivalent.
fn normalize_wire_node(node: Option<&str>) -> Option<String> {
    node.filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Connection-keyed [`ConnectionNotifier`] that turns liminal's in-band worker
/// registration into a first-class [`ConnectedWorkerRegistry`] membership.
///
/// This is the SERVER half of LSUB-L2: when a worker connects with a
/// [`WireWorkerRegistration`] (the SDK's `connect_with_registration`), liminal's
/// connection process invokes [`on_worker_registered`](Self::on_worker_registered)
/// with the connection's beamr `pid` and the worker's declared
/// `(namespaces, task_queue, node, activity_types)`. The notifier builds a
/// [`WorkerDelivery::Liminal`] over the connection and inserts it into the
/// registry — the SAME registry entry, selected the SAME way, as a gRPC worker —
/// retiring the LSUB-1 out-of-band `active_connection_pids()` + hard-coded
/// registration hack.
///
/// # Lifetime of the registration guard
///
/// [`ConnectedWorkerRegistry::register_delivery`] returns a
/// [`WorkerRegistration`] guard whose drop deregisters the worker. The notifier
/// OWNS that guard keyed by `pid` (`Mutex<HashMap<u64, WorkerRegistration>>`), so
/// the registration lives exactly as long as the connection: it is inserted on
/// register and removed (dropped) on
/// [`on_worker_unregistered`](Self::on_worker_unregistered), which liminal fires
/// on connection close.
///
/// # Construction-order cycle (notifier <-> supervisor)
///
/// [`Self::dispatch`'s delivery] needs a [`ConnectionSupervisor`] handle to push
/// to the worker's connection, but the supervisor is itself constructed WITH this
/// notifier ([`ConnectionSupervisor::with_services_and_notifier`]) — a cycle. The
/// notifier therefore holds the supervisor behind a [`OnceLock`], populated
/// IMMEDIATELY after the supervisor is built via [`Self::bind_supervisor`]. The
/// `OnceLock` is never read before it is set in correct wiring (a worker can only
/// register after the listener — built after the supervisor and after
/// `bind_supervisor` — accepts its connection); if it somehow were, registration
/// is REJECTED with a typed error rather than panicking, so there is no
/// production `unwrap`/`expect` and no second always-`None` code path.
pub struct LiminalConnectionNotifier {
    registry: ConnectedWorkerRegistry,
    supervisor: OnceLock<ConnectionSupervisor>,
    guards: Mutex<HashMap<u64, WorkerRegistration>>,
    /// The neutral intervention primitives a liminal-connected agent worker
    /// advertises (NOI-6, item 4). The liminal `WorkerRegistration` wire has a fixed
    /// shape that cannot carry this, so it is configured on the notifier at the
    /// composition root from the harness's advertised `AgentSession::capabilities()`
    /// and recorded on every registered worker's handle, where the intervention
    /// router gates on it. Default empty = observability-only (a plain activity
    /// worker), so the router offers no controls for it.
    intervention_capabilities: aion_core::InterventionCapabilities,
    /// The transcript sequencer a worker's observability publishes drain into
    /// (NOI-5b), plus the Tokio [`Handle`](tokio::runtime::Handle) to bridge the
    /// synchronous connection-process callback onto the async publish. `None` (the
    /// default, and every non-agent boot) makes the observability tap a no-op, so a
    /// worker publish to the reserved channel is simply ignored by the notifier.
    transcript: Option<TranscriptTap>,
}

/// The observability-drain leg of the notifier: the transcript sequencer to publish
/// into and the runtime handle used to spawn the async publish from the synchronous
/// `on_channel_publish` callback (which runs on the beamr connection-process thread).
#[derive(Clone)]
struct TranscriptTap {
    publisher: crate::activity_publisher::ActivityEventPublisher,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for LiminalConnectionNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiminalConnectionNotifier")
            .field("supervisor_bound", &self.supervisor.get().is_some())
            .finish_non_exhaustive()
    }
}

impl LiminalConnectionNotifier {
    /// Build a notifier that registers connecting workers into `registry`.
    ///
    /// The supervisor handle is bound separately via [`Self::bind_supervisor`]
    /// immediately after the supervisor is constructed, resolving the
    /// notifier <-> supervisor construction cycle (see the type docs).
    #[must_use]
    pub fn new(registry: ConnectedWorkerRegistry) -> Self {
        Self {
            registry,
            supervisor: OnceLock::new(),
            guards: Mutex::new(HashMap::new()),
            intervention_capabilities: aion_core::InterventionCapabilities::none(),
            transcript: None,
        }
    }

    /// Install the transcript sequencer a worker's observability publishes drain into
    /// (NOI-5b), capturing the CURRENT Tokio runtime handle to bridge the synchronous
    /// connection-process callback onto the async publish.
    ///
    /// MUST be called from within a Tokio runtime (the server boot path is), so the
    /// captured [`Handle`](tokio::runtime::Handle) can spawn the append+fan-out when a
    /// worker publishes a transcript event over the reserved channel. Without this
    /// builder the observability tap is a no-op (a plain, non-agent deployment).
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime — a construction-time wiring error in
    /// the server boot, never a runtime condition (the boot path always builds the
    /// notifier inside the server runtime).
    #[must_use]
    pub fn with_transcript_publisher(
        mut self,
        publisher: crate::activity_publisher::ActivityEventPublisher,
    ) -> Self {
        self.transcript = Some(TranscriptTap {
            publisher,
            runtime: tokio::runtime::Handle::current(),
        });
        self
    }

    /// Set the neutral intervention capability set every worker registering through
    /// this notifier advertises (NOI-6, item 4).
    ///
    /// The composition root wires this from the harness's advertised
    /// `AgentSession::capabilities()` so a liminal-connected agent worker's handle
    /// carries the primitives its harness supports, which the intervention router
    /// gates on. Without this builder the set is empty (observability-only), so a
    /// plain activity worker advertises no controls. Pure builder addition, mirroring
    /// the registry's capability-carrying registration façade.
    #[must_use]
    pub fn with_intervention_capabilities(
        mut self,
        capabilities: aion_core::InterventionCapabilities,
    ) -> Self {
        self.intervention_capabilities = capabilities;
        self
    }

    /// Bind the connection supervisor the notifier pushes through, immediately
    /// after it is constructed with this notifier.
    ///
    /// Returns `true` when the supervisor was stored, `false` when it was already
    /// bound (a second bind is a wiring bug and is ignored, never overwriting the
    /// live handle). Call this exactly once, right after
    /// [`ConnectionSupervisor::with_services_and_notifier`].
    pub fn bind_supervisor(&self, supervisor: ConnectionSupervisor) -> bool {
        self.supervisor.set(supervisor).is_ok()
    }
}

impl ConnectionNotifier for LiminalConnectionNotifier {
    fn on_worker_registered(
        &self,
        pid: u64,
        registration: &WireWorkerRegistration,
    ) -> Result<(), LiminalServerError> {
        // The delivery leg needs the supervisor to push to this connection. In
        // correct wiring it is bound before any connection is accepted; a missing
        // binding is a rejected registration, never a panic.
        let supervisor =
            self.supervisor
                .get()
                .ok_or_else(|| LiminalServerError::ListenerAccept {
                    message: format!(
                        "liminal worker registration for connection {pid} rejected: \
                     notifier supervisor handle not yet bound"
                    ),
                })?;

        let delivery = WorkerDelivery::Liminal(LiminalWorkerDelivery::new(supervisor.clone(), pid));
        let node = normalize_wire_node(registration.node.as_deref());
        // Insert into the SAME registry, selected the SAME way, as a gRPC worker.
        // A registry error (poisoned lock) becomes a Rejected ack so the worker
        // never believes it is registered when it is not.
        let guard = self
            .registry
            .register_delivery_with_capabilities(
                registration.namespaces.iter().cloned(),
                registration.task_queue.clone(),
                node,
                registration.activity_types.iter(),
                delivery,
                self.intervention_capabilities.clone(),
            )
            .map_err(|error| LiminalServerError::ListenerAccept {
                message: format!(
                    "liminal worker registration for connection {pid} rejected: {error}"
                ),
            })?;

        // OWN the guard for the connection's lifetime, keyed by pid. Dropping it
        // (on unregister) deregisters the worker, so the registration lives
        // exactly as long as the connection.
        let mut guards = self.guards.lock().map_err(|_| {
            // The accepted registry entry cannot be tracked for deregistration, so
            // reject (and drop the just-created guard, deregistering it) rather
            // than leak a never-deregistered association.
            LiminalServerError::ListenerAccept {
                message: format!(
                    "liminal worker registration for connection {pid} rejected: \
                     notifier guard map poisoned"
                ),
            }
        })?;
        guards.insert(pid, guard);
        tracing::info!(
            connection_pid = pid,
            identity = %registration.identity,
            task_queue = %registration.task_queue,
            "registered liminal worker in-band"
        );
        Ok(())
    }

    fn on_worker_unregistered(&self, pid: u64) {
        // Remove + drop the guard for pid, deregistering the worker. A poisoned
        // lock on the close path has no peer to report to; recover the guard map
        // and still drop the guard so the registry does not keep routing to a
        // gone connection.
        let removed = match self.guards.lock() {
            Ok(mut guards) => guards.remove(&pid),
            Err(poisoned) => poisoned.into_inner().remove(&pid),
        };
        if removed.is_some() {
            tracing::info!(
                connection_pid = pid,
                "deregistered liminal worker on disconnect"
            );
        }
    }

    fn on_channel_publish(&self, _pid: u64, channel: &str, payload: &[u8]) -> bool {
        // Only consume the reserved observability channel; any other channel falls
        // through to liminal's normal fan-out (this returns false).
        if channel != liminal_sdk::OBSERVABILITY_CHANNEL {
            return false;
        }
        let Some(tap) = &self.transcript else {
            // No transcript sequencer installed (a non-agent deployment): still
            // CONSUME the reserved channel so it never leaks into the fan-out, but
            // drop the event — there is nothing to persist it into.
            return true;
        };
        let event: aion_core::ActivityEvent = match serde_json::from_slice(payload) {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, "observability tap: malformed ActivityEvent payload");
                return true;
            }
        };
        // Bridge the synchronous connection-process callback onto the async
        // append+fan-out. The commit-allocated store_seq loop lives in `publish`;
        // a failed persist is logged, never retried (best-effort live streaming).
        let publisher = tap.publisher.clone();
        tap.runtime.spawn(async move {
            if let Err(error) = publisher.publish(&event).await {
                tracing::warn!(%error, "observability tap: transcript publish failed");
            }
        });
        true
    }
}

/// The production [`InterventionTransport`](super::intervention::InterventionTransport):
/// pushes a routed command to the owning worker over its liminal server-push
/// connection (NOI-6, §6.2).
///
/// It reads the worker handle's [`WorkerDelivery::Liminal`] leg and pushes the
/// neutral [`InterventionRequest`] via [`LiminalWorkerDelivery::push_intervention`],
/// running the blocking push off the async runtime. A worker delivered over gRPC
/// (no liminal leg) surfaces the stale-target no-op via a connection-lost error, so
/// the router NACKs the operator rather than routing to a leg it cannot reach.
#[derive(Clone, Debug, Default)]
pub struct LiminalInterventionTransport;

#[async_trait]
impl super::intervention::InterventionTransport for LiminalInterventionTransport {
    async fn push(
        &self,
        worker: &super::registry::WorkerHandle,
        command: aion_core::InterventionCommand,
    ) -> Result<aion_core::InterventionOutcome, ServerError> {
        let delivery = match worker.delivery() {
            WorkerDelivery::Liminal(delivery) => delivery.clone(),
            WorkerDelivery::Grpc(_) => {
                // No liminal leg to push to: the intervention transport rides the
                // liminal push channel only, so this is unreachable for the target.
                return Err(ServerError::worker_connection_lost(
                    "liminal-push",
                    "owning worker is not delivered over liminal".to_owned(),
                ));
            }
        };
        let request = InterventionRequest {
            intervention: command,
        };
        let reply = tokio::task::spawn_blocking(move || delivery.push_intervention(&request))
            .await
            .map_err(|error| {
                dispatch_error(
                    "liminal-push",
                    format!("intervention task join failed: {error}"),
                )
            })??;
        Ok(reply.outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::{channel_for_row, dispatch_channel_name, normalize_wire_node};
    use aion_core::{ActivityId, ContentType, Payload, WorkflowId};
    use aion_store::{OutboxRow, OutboxStatus};
    use chrono::Utc;
    use uuid::Uuid;

    /// The NOI-6 dispatch owner guard RELEASES its binding on drop, on EVERY exit path
    /// (reply, error, panic) — so the attempt-owner back-index tracks exactly the
    /// attempts currently in flight. This is the invariant the dispatch path relies on
    /// to never leak a finished attempt's owner.
    #[tokio::test]
    async fn attempt_owner_guard_releases_on_drop() -> Result<(), Box<dyn std::error::Error>> {
        use super::super::intervention::{AttemptKey, AttemptOwnerIndex};
        use super::super::registry::{ConnectedWorkerRegistry, WorkerDelivery};
        use super::AttemptOwnerGuard;

        // A real registration yields a real WorkerId (there is no fabricated id).
        let registry = ConnectedWorkerRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let types = [String::from("agent")];
        let registration = registry.register_delivery_with_capabilities(
            [String::from("default")],
            String::from("default"),
            None,
            types.iter(),
            WorkerDelivery::Grpc(tx),
            aion_core::InterventionCapabilities::none(),
        )?;
        let worker = registration
            .worker_id()
            .ok_or("registration must assign a worker id")?;

        let owners = AttemptOwnerIndex::new();
        let key = AttemptKey::new(
            WorkflowId::new(Uuid::nil()),
            ActivityId::from_sequence_position(3),
            1,
        );
        owners.bind(key.clone(), worker);
        assert_eq!(
            owners.owner(&key),
            Some(worker),
            "owner bound before the guard"
        );
        {
            let _guard = AttemptOwnerGuard {
                owners: owners.clone(),
                key: key.clone(),
            };
            assert_eq!(
                owners.owner(&key),
                Some(worker),
                "still bound while in flight"
            );
        }
        // The guard dropped at the end of the block: the binding is released, so a
        // later intervention resolves no owner (the too-late no-op).
        assert_eq!(
            owners.owner(&key),
            None,
            "owner released when the dispatch returns"
        );
        Ok(())
    }

    /// The channel format is pinned EXACTLY: any change is a wire-compatibility
    /// break (the dispatcher and any worker subscription must agree byte-for-byte).
    /// The UNPINNED (`None`) channel MUST stay byte-identical to the pre-NODE-5
    /// format so existing pool subscriptions are stable.
    #[test]
    fn channel_format_is_pinned() {
        assert_eq!(
            dispatch_channel_name("remote", "gpu", None),
            "aion.dispatch.remote.gpu"
        );
        assert_eq!(
            dispatch_channel_name("local", "norn", None),
            "aion.dispatch.local.norn"
        );
    }

    /// A node-pinned dispatch appends the node as an injectively-encoded
    /// sub-segment: `f(ns, tq, Some(node))` == `aion.dispatch.{ns}.{tq}.{node}`.
    #[test]
    fn node_pinned_channel_appends_node_subsegment() {
        assert_eq!(
            dispatch_channel_name("remote", "gpu", Some("box-7")),
            "aion.dispatch.remote.gpu.box-7"
        );
    }

    /// Same input always yields the same channel (the function is stable/total),
    /// for both the unpinned and node-pinned cases.
    #[test]
    fn channel_derivation_is_stable() {
        assert_eq!(
            dispatch_channel_name("default", "default", None),
            dispatch_channel_name("default", "default", None)
        );
        assert_eq!(
            dispatch_channel_name("default", "default", Some("box-1")),
            dispatch_channel_name("default", "default", Some("box-1"))
        );
    }

    /// Distinct `(namespace, task_queue)` pools derive distinct channels — the
    /// whole point of NSTQ-5: `(remote, gpu)` and `(local, norn)` never collide.
    #[test]
    fn distinct_pools_get_distinct_channels() {
        assert_ne!(
            dispatch_channel_name("remote", "gpu", None),
            dispatch_channel_name("local", "norn", None)
        );
    }

    /// A node-pinned dispatch and the unpinned dispatch for the SAME pool derive
    /// DISTINCT channels, and two distinct nodes for the same pool also differ —
    /// the property node isolation rests on (the subscriber contract).
    #[test]
    fn node_pin_separates_channels() {
        let unpinned = dispatch_channel_name("remote", "gpu", None);
        let box7 = dispatch_channel_name("remote", "gpu", Some("box-7"));
        let box8 = dispatch_channel_name("remote", "gpu", Some("box-8"));
        assert_ne!(
            unpinned, box7,
            "pinned dispatch must not reach unpinned pool"
        );
        assert_ne!(box7, box8, "distinct nodes must not collide");
    }

    /// The core injectivity property: free-form fields containing the segment
    /// separator `.` must NOT bleed across the join. With the raw `format!` the
    /// disjoint pools `("a.b", "c")` and `("a", "b.c")` both collapsed onto
    /// `aion.dispatch.a.b.c` — a cross-pool/cross-namespace leak. The per-segment
    /// encode keeps them distinct.
    #[test]
    fn dotted_fields_do_not_collide_across_segments() {
        assert_ne!(
            dispatch_channel_name("a.b", "c", None),
            dispatch_channel_name("a", "b.c", None),
            "a '.' in a field must not bleed across the segment separator"
        );
    }

    /// Injectivity holds ACROSS segment counts: a 2-segment (unpinned) channel
    /// can never be confused with a 3-segment (node-pinned) channel even when a
    /// `.` in a field would otherwise make the raw strings line up. Both
    /// directions of the brief's collision cases must stay distinct.
    #[test]
    fn node_subsegment_does_not_collide_with_dotted_fields() {
        // A node sub-segment vs the same dot living inside task_queue.
        assert_ne!(
            dispatch_channel_name("a", "b", Some("c")),
            dispatch_channel_name("a", "b.c", None),
            "a node sub-segment must not collide with a dotted task_queue"
        );
        // The dot living inside namespace vs a node sub-segment.
        assert_ne!(
            dispatch_channel_name("a.b", "c", None),
            dispatch_channel_name("a", "b", Some("c")),
            "a dotted namespace must not collide with a node-pinned channel"
        );
    }

    /// More reserved-char shifts that the raw `format!` collapsed but the encode
    /// must keep distinct — the dot can sit on either side of the boundary.
    #[test]
    fn reserved_char_shifts_stay_distinct() {
        // Dot at the end of namespace vs start of task_queue.
        assert_ne!(
            dispatch_channel_name("ns.", "tq", None),
            dispatch_channel_name("ns", ".tq", None)
        );
        // Empty field vs the dot living in the other field.
        assert_ne!(
            dispatch_channel_name("", "a.b", None),
            dispatch_channel_name(".a", "b", None)
        );
        // The escape char itself must not let a literal `%2E` impersonate an
        // encoded `.`: `("%2E", "x")` (literal percent-two-E) must differ from
        // `(".", "x")` (an actual dot, which encodes to `%2E`).
        assert_ne!(
            dispatch_channel_name("%2E", "x", None),
            dispatch_channel_name(".", "x", None)
        );
    }

    /// Encoding is injective in ALL THREE segments independently and is exactly
    /// reversible (the property the channel relies on), so a small exhaustive
    /// sweep of reserved-char arrangements — INCLUDING the optional node taking
    /// `None` and every reserved-char value — yields all-distinct channels. This
    /// covers cross-segment-count collisions (the `None` vs `Some` boundary) too.
    #[test]
    fn encoding_is_injective_over_reserved_char_triples() {
        let fields = ["a", "a.b", "a.", ".a", ".", "", "%", "%2E", "a%b", "%2."];
        let nodes = [
            None,
            Some("a"),
            Some("a.b"),
            Some("."),
            Some(""),
            Some("%2E"),
        ];
        let mut channels = std::collections::HashSet::new();
        for ns in fields {
            for tq in fields {
                for node in nodes {
                    let channel = dispatch_channel_name(ns, tq, node);
                    assert!(
                        channels.insert(channel.clone()),
                        "collision on ({ns:?}, {tq:?}, {node:?}) -> {channel}"
                    );
                }
            }
        }
    }

    fn row(namespace: &str, task_queue: &str) -> OutboxRow {
        let workflow_id = WorkflowId::new(Uuid::new_v4());
        OutboxRow {
            dispatch_key: format!("{workflow_id}:0"),
            workflow_id,
            ordinal: 0,
            run_id: None,
            namespace: namespace.to_owned(),
            task_queue: task_queue.to_owned(),
            node: None,
            activity_type: "charge-card".to_owned(),
            input: Payload::new(ContentType::Json, Vec::new()),
            status: OutboxStatus::Pending,
            attempt: 0,
            visible_after: Utc::now(),
            claimed_at: None,
        }
    }

    /// A row's channel is derived from its durable `(namespace, task_queue)`
    /// columns (NSTQ-2), through the same single derivation function — and
    /// `activity_type` does NOT enter the channel. With `node = None` the channel
    /// is byte-identical to the pre-NODE-5 2-segment form.
    #[test]
    fn channel_for_row_uses_namespace_and_task_queue_only() {
        let remote_gpu = row("remote", "gpu");
        let local_norn = row("local", "norn");
        assert_eq!(channel_for_row(&remote_gpu), "aion.dispatch.remote.gpu");
        assert_eq!(channel_for_row(&local_norn), "aion.dispatch.local.norn");
        assert_ne!(channel_for_row(&remote_gpu), channel_for_row(&local_norn));

        // Two rows that differ ONLY in activity_type derive the SAME channel:
        // activity_type is matched after delivery, not used to select the pool.
        let mut other_activity = row("remote", "gpu");
        other_activity.activity_type = "refund".to_owned();
        assert_eq!(
            channel_for_row(&remote_gpu),
            channel_for_row(&other_activity),
            "activity_type must not affect the channel"
        );
    }

    /// A row carrying `Some(node)` (NODE-2) derives the node-pinned sub-channel,
    /// distinct from the same pool's unpinned channel; a row with `None` derives
    /// the 2-segment channel. `channel_for_row` threads `row.node` through the
    /// single derivation function.
    #[test]
    fn channel_for_row_derives_node_subchannel_when_pinned() {
        let mut pinned = row("remote", "gpu");
        pinned.node = Some("box-7".to_owned());
        assert_eq!(channel_for_row(&pinned), "aion.dispatch.remote.gpu.box-7");

        let unpinned = row("remote", "gpu");
        assert_eq!(channel_for_row(&unpinned), "aion.dispatch.remote.gpu");
        assert_ne!(channel_for_row(&pinned), channel_for_row(&unpinned));
    }

    /// The wire `node` is normalized onto the registry's optional affinity with
    /// the SAME none-convention the gRPC registration path uses: `None` and the
    /// empty-string node both collapse to unpinned (`None`), a non-empty value is
    /// the advertised node. An empty-string node must NOT register a distinct
    /// empty affinity no pinned dispatch could match.
    #[test]
    fn wire_node_normalizes_empty_to_none() {
        assert_eq!(normalize_wire_node(None), None);
        assert_eq!(normalize_wire_node(Some("")), None);
        assert_eq!(normalize_wire_node(Some("box-7")), Some("box-7".to_owned()));
    }

    // --- #163: the Prefer two-tier spill on the LIMINAL selection path ---------
    //
    // These exercise `RegistryLiminalDispatch::select_liminal_worker` — the
    // liminal transport's worker selection — proving it consults the SAME shared
    // `preferred_node_order` two-tier spill the gRPC path uses (the cross-node
    // demo behaviour), and that placement NEVER mutates the recorded row's node.
    // Selection is delivery-agnostic (`select_worker` filters by node regardless
    // of transport), so a worker registered with any delivery drives the same
    // selection the production liminal-delivered worker would; the tests assert on
    // the SELECTED handle's node, which is exactly what #163 changed.
    mod placement_selection {
        use std::collections::BTreeSet;
        use std::sync::Arc;
        use std::time::Duration;

        use aion_core::{ActivityId, Payload, RunId, WorkflowId};
        use aion_store::{
            InMemoryStore, NamespaceOrigin, NamespacePlacement, NamespaceStore, OutboxRow,
        };

        use crate::error::ServerError;
        use crate::worker::PlacementCache;
        use crate::worker::bridge::OutboxDeliveryCallback;
        use crate::worker::registry::{ConnectedWorkerRegistry, WorkerMessage, WorkerRegistration};

        use super::super::RegistryLiminalDispatch;

        /// No-op delivery callback: the selection tests never deliver a result, so
        /// the completion sink is never invoked. Both methods are unreachable in
        /// these tests and simply report "no live run" if ever called.
        struct NoopCallback;

        impl OutboxDeliveryCallback for NoopCallback {
            fn deliver_completion(
                &self,
                _workflow_id: &WorkflowId,
                _activity_id: &ActivityId,
                _run_id: Option<&RunId>,
                _result: String,
            ) -> Result<bool, ServerError> {
                Ok(false)
            }
            fn deliver_failure(
                &self,
                _workflow_id: &WorkflowId,
                _activity_id: &ActivityId,
                _run_id: Option<&RunId>,
                _reason: String,
            ) -> Result<bool, ServerError> {
                Ok(false)
            }
        }

        fn labels(values: &[&str]) -> BTreeSet<String> {
            values.iter().map(|v| (*v).to_owned()).collect()
        }

        /// Register a worker advertising `node` for `charge` in `namespace`,
        /// returning the registration guard (held to keep it connected).
        fn register_node_worker(
            registry: &ConnectedWorkerRegistry,
            namespace: &str,
            node: &str,
        ) -> Result<WorkerRegistration, ServerError> {
            let (tx, _rx) = tokio::sync::mpsc::channel::<WorkerMessage>(1);
            let types = [String::from("charge")];
            registry.register_namespaces(
                [namespace.to_owned()],
                String::from("default"),
                Some(node.to_owned()),
                types.iter(),
                tx,
            )
        }

        /// Build an UNPINNED outbox row (`node == None`) in `namespace` for `charge`.
        fn unpinned_row(namespace: &str) -> OutboxRow {
            OutboxRow::pending(
                WorkflowId::new_v4(),
                0,
                String::from("charge"),
                Payload::from_json(&serde_json::json!({}))
                    .unwrap_or_else(|_| Payload::new(aion_core::ContentType::Json, Vec::new())),
                chrono::Utc::now(),
            )
            .with_namespace(namespace)
            .with_task_queue("default")
        }

        /// A namespace store with `namespace` set to `Prefer{nodes}`.
        async fn prefer_store(
            namespace: &str,
            nodes: &[&str],
        ) -> Result<Arc<dyn NamespaceStore>, ServerError> {
            let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
            store
                .register_namespace(namespace, NamespaceOrigin::Explicit)
                .await?;
            store
                .set_namespace_placement(
                    namespace,
                    NamespacePlacement::Prefer {
                        nodes: labels(nodes),
                    },
                )
                .await?;
            Ok(store)
        }

        /// A namespace store with `namespace` set to `Pinned{nodes}` (P2-I1).
        async fn pinned_store(
            namespace: &str,
            nodes: &[&str],
        ) -> Result<Arc<dyn NamespaceStore>, ServerError> {
            let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
            store
                .register_namespace(namespace, NamespaceOrigin::Explicit)
                .await?;
            store
                .set_namespace_placement(
                    namespace,
                    NamespacePlacement::Pinned {
                        nodes: labels(nodes),
                    },
                )
                .await?;
            Ok(store)
        }

        /// Build a `RegistryLiminalDispatch` over `registry` whose placement cache
        /// reads `ns_store` (zero TTL so each selection sees the latest placement).
        fn liminal_dispatch(
            registry: &ConnectedWorkerRegistry,
            ns_store: Arc<dyn NamespaceStore>,
        ) -> RegistryLiminalDispatch {
            let cache = PlacementCache::new(ns_store, Duration::ZERO);
            RegistryLiminalDispatch::new(registry.clone(), Arc::new(NoopCallback))
                .with_placement_cache(cache)
        }

        /// #163 (prefer): an unpinned row in a `Prefer{n1}` namespace selects the
        /// n1 worker on the liminal path when one is live, even with an n2 worker
        /// also connected.
        #[tokio::test]
        async fn prefer_selects_preferred_node_worker_on_liminal_path()
        -> Result<(), Box<dyn std::error::Error>> {
            let ns_store = prefer_store("t", &["n1"]).await?;
            let registry = ConnectedWorkerRegistry::default();
            let _n1 = register_node_worker(&registry, "t", "n1")?;
            let _n2 = register_node_worker(&registry, "t", "n2")?;
            let dispatch = liminal_dispatch(&registry, Arc::clone(&ns_store));

            let row = unpinned_row("t");
            let selected = dispatch
                .select_liminal_worker(&row)
                .await?
                .ok_or("a worker must be selected")?;
            assert_eq!(
                selected.node(),
                Some("n1"),
                "the liminal path prefers the n1 worker while it is live"
            );
            // Determinism gate: preference never mutates the recorded row's node.
            assert_eq!(row.node, None, "placement must never mutate the row's node");
            Ok(())
        }

        /// #163 (spill): an unpinned row in a `Prefer{n1}` namespace SPILLS to the
        /// only live worker (n2) on the liminal path when no n1 worker is
        /// connected — the cross-node node-loss failover behaviour.
        #[tokio::test]
        async fn prefer_spills_to_any_live_worker_on_liminal_path()
        -> Result<(), Box<dyn std::error::Error>> {
            let ns_store = prefer_store("t", &["n1"]).await?;
            // Only an n2 worker is live: no n1-labelled worker exists at all.
            let registry = ConnectedWorkerRegistry::default();
            let _n2 = register_node_worker(&registry, "t", "n2")?;
            let dispatch = liminal_dispatch(&registry, Arc::clone(&ns_store));

            let row = unpinned_row("t");
            let selected = dispatch
                .select_liminal_worker(&row)
                .await?
                .ok_or("the spill must select the live n2 worker")?;
            assert_eq!(
                selected.node(),
                Some("n2"),
                "with no n1 worker live, the liminal selection spills to the live n2 worker"
            );
            assert_eq!(row.node, None, "spill must never mutate the row's node");
            Ok(())
        }

        /// #163 (determinism, mirrors the gRPC `placement_never_mutates_recorded_row_node`
        /// test): under `Prefer{n1}` the SAME unpinned row selected once to the n1
        /// worker and once (after n1 leaves) spilled to n2 keeps `node == None`
        /// BOTH times — selection reads the row's node, never the placement, so
        /// replay sees an identical command stream irrespective of the target.
        #[tokio::test]
        async fn placement_never_mutates_recorded_row_node_on_liminal_path()
        -> Result<(), Box<dyn std::error::Error>> {
            let ns_store = prefer_store("t", &["n1"]).await?;
            let registry = ConnectedWorkerRegistry::default();
            let dispatch = liminal_dispatch(&registry, Arc::clone(&ns_store));

            // Routing A: n1 present -> preferred selection.
            let n1 = register_node_worker(&registry, "t", "n1")?;
            let row_a = unpinned_row("t");
            let selected_a = dispatch
                .select_liminal_worker(&row_a)
                .await?
                .ok_or("routing A must select a worker")?;
            assert_eq!(selected_a.node(), Some("n1"));

            // n1 leaves; only n2 remains.
            n1.deregister()?;
            let _n2 = register_node_worker(&registry, "t", "n2")?;

            // Routing B: same shape of unpinned row -> spills to n2.
            let row_b = unpinned_row("t");
            let selected_b = dispatch
                .select_liminal_worker(&row_b)
                .await?
                .ok_or("routing B must spill to a worker")?;
            assert_eq!(selected_b.node(), Some("n2"));

            // The recorded row node is None in BOTH routings: the dispatch target
            // (n1 vs n2) did not perturb it.
            assert_eq!(row_a.node, None);
            assert_eq!(row_b.node, None);
            assert_eq!(
                row_a.node, row_b.node,
                "the recorded row node is identical regardless of which worker was selected"
            );
            Ok(())
        }

        /// #164 (P2-I1 hard pin): an unpinned row in a `Pinned{n1}` namespace
        /// selects the n1 worker on the liminal path when live — exactly like
        /// Prefer's happy path.
        #[tokio::test]
        async fn pinned_selects_required_node_worker_on_liminal_path()
        -> Result<(), Box<dyn std::error::Error>> {
            let ns_store = pinned_store("t", &["n1"]).await?;
            let registry = ConnectedWorkerRegistry::default();
            let _n1 = register_node_worker(&registry, "t", "n1")?;
            let _n2 = register_node_worker(&registry, "t", "n2")?;
            let dispatch = liminal_dispatch(&registry, Arc::clone(&ns_store));

            let row = unpinned_row("t");
            let selected = dispatch
                .select_liminal_worker(&row)
                .await?
                .ok_or("the required n1 worker must be selected")?;
            assert_eq!(selected.node(), Some("n1"));
            assert_eq!(row.node, None, "placement must never mutate the row's node");
            Ok(())
        }

        /// #164 (P2-I1 no spill — the load-bearing test): an unpinned row in a
        /// `Pinned{n1}` namespace with ONLY a live n2 worker selects NOTHING — it
        /// must NEVER spill to the wrong-node worker. This is the exact opposite of
        /// the `prefer_spills_to_any_live_worker_on_liminal_path` behaviour and would
        /// FAIL under the old fall-through (which selected any worker for Pinned).
        /// The `Ok(None)` drives the outbox no-worker retry/stall, mirroring the
        /// gRPC wait.
        #[tokio::test]
        async fn pinned_never_spills_to_a_wrong_node_worker_on_liminal_path()
        -> Result<(), Box<dyn std::error::Error>> {
            let ns_store = pinned_store("t", &["n1"]).await?;
            // Only an n2 worker is live: no n1-labelled worker exists at all.
            let registry = ConnectedWorkerRegistry::default();
            let _n2 = register_node_worker(&registry, "t", "n2")?;
            let dispatch = liminal_dispatch(&registry, Arc::clone(&ns_store));

            let row = unpinned_row("t");
            let selected = dispatch.select_liminal_worker(&row).await?;
            assert!(
                selected.is_none(),
                "Pinned{{n1}} must NOT spill to the live n2 worker — it selects nothing \
                 so the outbox retries/stalls until an n1 worker returns"
            );
            assert_eq!(row.node, None, "placement must never mutate the row's node");
            Ok(())
        }

        /// #163 (authored pin wins): a row authored-pinned to `Some(n2)` STILL
        /// selects an n2 worker on the liminal path regardless of the namespace's
        /// `Prefer{n1}` — the per-activity pin is authoritative and placement never
        /// overrides it.
        #[tokio::test]
        async fn authored_node_pin_wins_over_namespace_prefer_on_liminal_path()
        -> Result<(), Box<dyn std::error::Error>> {
            let ns_store = prefer_store("t", &["n1"]).await?;
            let registry = ConnectedWorkerRegistry::default();
            let _n1 = register_node_worker(&registry, "t", "n1")?;
            let _n2 = register_node_worker(&registry, "t", "n2")?;
            let dispatch = liminal_dispatch(&registry, Arc::clone(&ns_store));

            // Authored pin: node = Some("n2").
            let row = unpinned_row("t").with_node(Some(String::from("n2")));
            let selected = dispatch
                .select_liminal_worker(&row)
                .await?
                .ok_or("the authored pin must select the n2 worker")?;
            assert_eq!(
                selected.node(),
                Some("n2"),
                "the authored Some(n2) pin is honoured regardless of the namespace Prefer{{n1}}"
            );
            // The authored node is preserved exactly (determinism gate).
            assert_eq!(row.node.as_deref(), Some("n2"));
            Ok(())
        }

        /// #163 (byte-identical default): an `Unplaced` namespace selects any live
        /// worker on the liminal path exactly as the pre-Phase-2 single
        /// `select_worker` would — the ceiling/placement never engages.
        #[tokio::test]
        async fn unplaced_namespace_selects_any_worker_on_liminal_path()
        -> Result<(), Box<dyn std::error::Error>> {
            let ns_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
            // Registered but left Unplaced (the default placement).
            ns_store
                .register_namespace("t", NamespaceOrigin::Explicit)
                .await?;
            let registry = ConnectedWorkerRegistry::default();
            let _n2 = register_node_worker(&registry, "t", "n2")?;
            let dispatch = liminal_dispatch(&registry, Arc::clone(&ns_store));

            let selected = dispatch
                .select_liminal_worker(&unpinned_row("t"))
                .await?
                .ok_or("an Unplaced namespace still selects a live worker")?;
            assert_eq!(
                selected.node(),
                Some("n2"),
                "an Unplaced namespace reaches any live worker, exactly as before"
            );
            Ok(())
        }

        /// #163 (byte-identical, no cache): with NO placement cache attached, the
        /// liminal selection is the single `select_worker` off the row's own node —
        /// byte-identical to the pre-#163 construction. An unpinned row reaches any
        /// live worker; the namespace's `Prefer` is not even consulted.
        #[tokio::test]
        async fn no_cache_selection_is_byte_identical_to_pre_163()
        -> Result<(), Box<dyn std::error::Error>> {
            // The namespace prefers n1, but with no cache the preference is ignored.
            let _ns_store = prefer_store("t", &["n1"]).await?;
            let registry = ConnectedWorkerRegistry::default();
            let _n2 = register_node_worker(&registry, "t", "n2")?;
            // No `.with_placement_cache(...)`: the pre-#163 construction.
            let dispatch = RegistryLiminalDispatch::new(registry.clone(), Arc::new(NoopCallback));

            let selected = dispatch
                .select_liminal_worker(&unpinned_row("t"))
                .await?
                .ok_or("without a cache the unpinned row still selects any worker")?;
            assert_eq!(
                selected.node(),
                Some("n2"),
                "with no placement cache the selection is the unchanged any-worker path"
            );
            Ok(())
        }
    }
}
