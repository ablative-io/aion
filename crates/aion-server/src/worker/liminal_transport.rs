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
use liminal_server::server::connection::{ConnectionNotifier, ConnectionSupervisor};
use serde::{Deserialize, Serialize};

use super::bridge::OutboxDeliveryCallback;
use super::outbox_dispatcher::OutboxRowDispatch;
use super::registry::{ConnectedWorkerRegistry, WorkerDelivery, WorkerRegistration};
use crate::error::ServerError;

/// Upper bound on how long a server-initiated dispatch push waits for the
/// worker's correlated reply before the row is treated as undelivered and the
/// outbox retries. Generous because an activity may legitimately run a while; the
/// outbox's own retry/reconcile loop is the real liveness backstop.
const PUSH_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

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
        let payload = serde_json::to_vec(request).map_err(|error| {
            dispatch_error("liminal-push", format!("request serialize failed: {error}"))
        })?;
        // A push-enqueue failure has exactly one cause in liminal — the connection
        // process is not live (the worker is already gone) — so the whole arm is a
        // lost connection, re-armed for immediate failover by the outbox.
        let awaiter = self
            .supervisor
            .push_to_connection(self.pid, payload)
            .map_err(|error| {
                ServerError::worker_connection_lost(
                    "liminal-push",
                    format!("push to worker failed: {error}"),
                )
            })?;
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
        serde_json::from_slice(&reply).map_err(|error| {
            dispatch_error(
                "liminal-push",
                format!("worker reply decode failed: {error}"),
            )
        })
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
}

impl std::fmt::Debug for RegistryLiminalDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryLiminalDispatch")
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
        }
    }
}

#[async_trait]
impl OutboxRowDispatch for RegistryLiminalDispatch {
    async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError> {
        // Select the worker the SAME way the gRPC path does: by the row's
        // (namespace, task_queue, activity_type) pool key with the row's optional
        // node affinity. No worker for the pool => honest no-worker error => the
        // outbox retries (never a false `done`).
        let worker = self
            .registry
            .select_worker(
                &row.namespace,
                &row.task_queue,
                &row.activity_type,
                row.node.as_deref(),
            )?
            .ok_or_else(|| {
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
        }
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
            .register_delivery(
                registration.namespaces.iter().cloned(),
                registration.task_queue.clone(),
                node,
                registration.activity_types.iter(),
                delivery,
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
}

#[cfg(test)]
mod tests {
    use super::{channel_for_row, dispatch_channel_name, normalize_wire_node};
    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{OutboxRow, OutboxStatus};
    use chrono::Utc;
    use uuid::Uuid;

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
}
