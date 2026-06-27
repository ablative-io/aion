//! Cross-node outbox dispatch over the liminal bus (#13-0 spike).
//!
//! # What this is (bounded spike)
//!
//! This module wires ONE outbox fan-out dispatch over liminal to a worker and
//! returns the worker's result through the existing
//! [`OutboxDeliveryCallback`](super::bridge::OutboxDeliveryCallback), behind the
//! `liminal-transport` Cargo feature and the `outbox.transport = liminal`
//! runtime flag. It is deliberately the smallest useful slice:
//!
//! - **Per-row channel addressing (13-3 / NSTQ-5).** One liminal server address,
//!   provided to [`LiminalOutboxDispatch::new`]; the channel is derived
//!   **per dispatch** from the row's `(namespace, task_queue)` via
//!   [`dispatch_channel_name`] — the worker-pool address. `activity_type` is NOT
//!   a routing dimension: it rides inside the [`DispatchRequest`] payload and is
//!   matched by the worker after delivery (see `DispatchRequest::activity_type`),
//!   exactly as the gRPC registry pushes `activity_type` in the task body while
//!   selecting the worker by pool key. See
//!   `docs/NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md` §4.2 for why the earlier
//!   `(namespace, activity_type)` channel proposal was wrong.
//! - **Happy path, one worker.** A single dispatch + result round-trip. Retry
//!   through the honest delivery ack is exercised (the dispatch-out contract)
//!   but the wider retry/backoff/dead-letter proof is 13-1.
//! - **Per-attempt idempotency key.** A PER-ATTEMPT idempotency key
//!   (`{dispatch_key}#{attempt}`, both already on the row) keys liminal
//!   dedup-on-delivery. The `namespace` + `task_queue` columns the channel
//!   derivation reads were added by NSTQ-2 (the original "no `namespace` column"
//!   note predates that landed schema change).
//!
//! # The two seams it implements
//!
//! - [`LiminalOutboxDispatch`] implements
//!   [`OutboxRowDispatch`](super::outbox_dispatcher::OutboxRowDispatch): it maps
//!   an [`OutboxRow`] to a [`DispatchRequest`] and publishes it over liminal with
//!   a per-attempt idempotency key (`{dispatch_key}#{attempt}`, see
//!   [`attempt_idempotency_key`]), via `publish_with_idempotency_key`. It returns
//!   `Ok(())` ONLY when the returned
//!   [`DeliveryAck::is_accepted`] is `true` (a worker genuinely received it);
//!   otherwise it returns a [`ServerError::WorkerDispatch`] so the outbox's
//!   existing retry/backoff/dead-letter path drives the row, exactly as the gRPC
//!   path does on a failed push.
//! - [`LiminalCompletionSource`] receives the worker's [`DispatchResponse`] over
//!   the liminal conversation request-reply path and calls
//!   [`OutboxDeliveryCallback::deliver_completion`] /
//!   [`OutboxDeliveryCallback::deliver_failure`], threading `run_id` end-to-end
//!   so the existing continue-as-new run gates apply unchanged.
//!
//! # Honest scope note on the liminal wire (the integration gap)
//!
//! Liminal's 13-L0 request-reply round-trip is served by an in-server echo
//! participant (`liminal-server`'s conversation supervisor spawns an
//! `EchoBehaviour` responder); there is no API yet to register an *external*
//! aion-worker process as the conversation responder over the wire. So in 13-0
//! the "worker" that returns the result is liminal's echo participant: the WIRE
//! is genuine (real TCP, real correlation, real dedup-on-delivery, real delivery
//! ack), but the responder identity is liminal's echo, not a separate aion
//! worker binary. Registering a real remote aion worker as the responder is the
//! deferred work tracked by 13-6 and a corresponding liminal worker-pool seam.
//! This module's types and contracts are written so that swap is a change of
//! responder, not of the aion-side wiring.
//!
//! # The worker-subscription seam (out of scope here, documented)
//!
//! This module owns the DISPATCHER side: it publishes a row to the channel
//! [`dispatch_channel_name`] derives from the row's `(namespace, task_queue)`.
//! The SUBSCRIBER side — a remote aion-worker pool joining the liminal pg group
//! for the channel it serves — lives in the worker/liminal layer, not here, and
//! does not exist yet (13-0 uses liminal's in-server echo responder). When that
//! worker-pool transport is built, a worker pool addressed `(namespace,
//! task_queue)` MUST subscribe to the channel produced by **this same**
//! [`dispatch_channel_name`] function so the dispatcher and subscriber strings
//! cannot drift. That is the single contract the seam must honour.

use std::sync::Arc;

use aion_core::{ActivityId, ContentType, Payload, RunId, WorkflowId};
use aion_store::OutboxRow;
use async_trait::async_trait;
use liminal_sdk::{
    ConnectionPoolConfig, DeliveryAck, RemoteChannelHandle, RemoteConfig, SchemaMetadata,
    SchemaValidate,
};
use serde::{Deserialize, Serialize};

use super::bridge::OutboxDeliveryCallback;
use super::outbox_dispatcher::OutboxRowDispatch;
use crate::error::ServerError;

/// Default connection-pool sizing for the spike's single hard-coded connection.
///
/// One connection is enough for the one-worker happy path; the timeout and
/// buffer mirror the values liminal's own TCP e2e test uses.
const SPIKE_POOL: ConnectionPoolConfig = ConnectionPoolConfig::new(1, 10, 16);

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

/// Builds the per-attempt liminal idempotency key for one outbox row.
///
/// The outbox `dispatch_key` is stable across every retry of the same row, but
/// liminal's dedup-on-delivery claims a key at the first publish. Composing the
/// stable key with the row's zero-based `attempt` (`{dispatch_key}#{attempt}`)
/// gives each retry a distinct key, so a legitimate re-dispatch is a fresh,
/// non-suppressed publish while a true duplicate of the same attempt is still
/// deduped. The exactly-once authority remains aion's terminal dedup, not this
/// key.
///
/// Kept free-standing (not a method) so both the dispatch path and tests derive
/// the key identically.
#[must_use]
pub fn attempt_idempotency_key(row: &OutboxRow) -> String {
    format!("{}#{}", row.dispatch_key, row.attempt)
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
/// `(namespace, task_queue)`.
///
/// This is the **single, total source of truth** for the channel string: every
/// site that needs the channel a `(namespace, task_queue)` pool dispatches to —
/// both this dispatcher and any future worker-pool subscription side — MUST call
/// this function so the two sides cannot drift. The format is
/// `"aion.dispatch.{namespace}.{task_queue}"` where each `{segment}` is
/// independently passed through [`encode_segment`].
///
/// # Injectivity (why the per-segment encode matters)
///
/// `namespace` and `task_queue` are free-form (the design forbids preset
/// categories), so a raw `format!` would be NON-injective: a `.` inside either
/// field bleeds across the separator and two pools the design declares disjoint
/// collide onto one channel — e.g. `("a.b", "c")` and `("a", "b.c")` both yield
/// `aion.dispatch.a.b.c`, a cross-pool leak on the very isolation dimension this
/// routing exists to keep separate. Encoding each segment independently (the
/// separator `.` and the escape `%` are escaped within a segment) makes the map
/// from `(namespace, task_queue)` to channel string injective: distinct pairs
/// always yield distinct channels.
///
/// # Forward-compat (composes with an Nth segment)
///
/// Each segment is encoded on its own and the segments are joined with the
/// single separator, so adding a later optional segment (e.g. NODE-5's
/// `aion.dispatch.{ns}.{tq}.{node}`) is a trivial extension that stays injective
/// by the same argument — no field can ever produce a separator that bleeds into
/// the next segment.
///
/// `activity_type` is deliberately NOT part of the channel: it is *what to run*,
/// matched by the worker after delivery (it rides inside [`DispatchRequest`]),
/// not *which pool* — see `docs/NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md` §4.2. The
/// function is total (defined for every string pair) and stable (the same
/// `(namespace, task_queue)` always yields the same channel).
#[must_use]
pub fn dispatch_channel_name(namespace: &str, task_queue: &str) -> String {
    let namespace = encode_segment(namespace);
    let task_queue = encode_segment(task_queue);
    format!("aion.dispatch.{namespace}.{task_queue}")
}

/// Derives the liminal dispatch channel for a claimed outbox row.
///
/// Thin wrapper over [`dispatch_channel_name`] reading the row's durable
/// `(namespace, task_queue)` (NSTQ-2 columns). Kept free-standing so the
/// dispatch path and tests derive the row's channel identically.
#[must_use]
pub fn channel_for_row(row: &OutboxRow) -> String {
    dispatch_channel_name(&row.namespace, &row.task_queue)
}

/// Cross-node [`OutboxRowDispatch`] that places a claimed row over liminal.
///
/// Holds the liminal server address; the channel is derived **per dispatch**
/// from each row's `(namespace, task_queue)` via [`channel_for_row`], so a row
/// for `(remote, gpu)` and a row for `(local, norn)` publish to distinct pool
/// channels through one dispatcher. Each dispatch opens a fresh
/// [`RemoteChannelHandle`] (one connection, happy path); connection
/// pooling/reuse is a later increment.
pub struct LiminalOutboxDispatch {
    server_address: String,
}

impl std::fmt::Debug for LiminalOutboxDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiminalOutboxDispatch")
            .field("server_address", &self.server_address)
            .finish()
    }
}

impl LiminalOutboxDispatch {
    /// Build a liminal dispatch over a server address; the channel is derived
    /// per-row at dispatch time, not fixed at construction.
    #[must_use]
    pub fn new(server_address: impl Into<String>) -> Self {
        Self {
            server_address: server_address.into(),
        }
    }

    /// Connects a remote channel handle to the configured liminal server on the
    /// row-derived `channel`.
    fn connect(&self, channel: &str) -> Result<RemoteChannelHandle, ServerError> {
        let config = RemoteConfig::new(
            self.server_address.clone(),
            channel.to_owned(),
            channel.to_owned(),
            SPIKE_POOL,
        )
        .map_err(|error| dispatch_error(channel, format!("remote config invalid: {error}")))?;
        let connected = config
            .connect_tcp()
            .map_err(|error| dispatch_error(channel, format!("connect failed: {error}")))?;
        RemoteChannelHandle::new(&connected)
            .map_err(|error| dispatch_error(channel, format!("handle build failed: {error}")))
    }
}

/// Wraps a reason in the existing worker-dispatch error so a non-accepted send
/// drives the outbox's unchanged retry/backoff/dead-letter path. The row-derived
/// `channel` is surfaced as the `activity_type` field for operator diagnostics
/// (the field is a free-form context string on this transport's error).
fn dispatch_error(channel: &str, reason: String) -> ServerError {
    ServerError::WorkerDispatch {
        namespace: "liminal".to_owned(),
        activity_type: channel.to_owned(),
        reason,
    }
}

#[async_trait]
impl OutboxRowDispatch for LiminalOutboxDispatch {
    async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError> {
        // Derive the worker-pool channel from the row's durable
        // (namespace, task_queue) — the single addressing source (NSTQ-5).
        let channel = channel_for_row(row);
        let handle = self.connect(&channel)?;
        let request = request_for_row(row);
        // Use a PER-ATTEMPT idempotency key (`{dispatch_key}#{attempt}`) so each
        // outbox retry is a fresh liminal publish that dedup-on-delivery does NOT
        // suppress. The stable `dispatch_key` alone would be claimed at the first
        // attempt and every legitimate retry would come back non-accepted —
        // indistinguishable from "reached no worker" — burning the attempt budget
        // and dead-lettering a row that should have re-run (13-0's known trap).
        //
        // This does not weaken correctness: liminal dedup still suppresses a true
        // duplicate of the SAME attempt (e.g. a transport-level resend), and the
        // exactly-once authority is aion's terminal dedup
        // (`record_fan_out_completion`, idempotent on the dispatch_key/ordinal),
        // which never moves to liminal. Net contract: at-least-once delivery to
        // the worker, effectively-once terminal recording — unchanged from today.
        let idempotency_key = attempt_idempotency_key(row);
        let ack: DeliveryAck = handle
            .publish_with_idempotency_key(&request, &idempotency_key)
            .map_err(|error| dispatch_error(&channel, format!("publish failed: {error}")))?;
        // The load-bearing contract: treat the send as done ONLY on a genuine
        // delivery ack (a worker received it). With per-attempt keys a non-accept
        // now means an empty channel (no worker), so the outbox's retry/backoff is
        // the correct response — a legitimate retry is no longer self-suppressed.
        if ack.is_accepted() {
            Ok(())
        } else {
            Err(dispatch_error(
                &channel,
                "liminal delivery ack reported the publish reached no worker (empty channel)"
                    .to_owned(),
            ))
        }
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

#[cfg(test)]
mod tests {
    use super::{channel_for_row, dispatch_channel_name};
    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{OutboxRow, OutboxStatus};
    use chrono::Utc;
    use uuid::Uuid;

    /// The channel format is pinned EXACTLY: any change is a wire-compatibility
    /// break (the dispatcher and any worker subscription must agree byte-for-byte).
    #[test]
    fn channel_format_is_pinned() {
        assert_eq!(
            dispatch_channel_name("remote", "gpu"),
            "aion.dispatch.remote.gpu"
        );
        assert_eq!(
            dispatch_channel_name("local", "norn"),
            "aion.dispatch.local.norn"
        );
    }

    /// Same input always yields the same channel (the function is stable/total).
    #[test]
    fn channel_derivation_is_stable() {
        assert_eq!(
            dispatch_channel_name("default", "default"),
            dispatch_channel_name("default", "default")
        );
    }

    /// Distinct `(namespace, task_queue)` pools derive distinct channels — the
    /// whole point of NSTQ-5: `(remote, gpu)` and `(local, norn)` never collide.
    #[test]
    fn distinct_pools_get_distinct_channels() {
        assert_ne!(
            dispatch_channel_name("remote", "gpu"),
            dispatch_channel_name("local", "norn")
        );
    }

    /// The core injectivity property: free-form fields containing the segment
    /// separator `.` must NOT bleed across the join. With the raw `format!` the
    /// disjoint pools `("a.b", "c")` and `("a", "b.c")` both collapsed onto
    /// `aion.dispatch.a.b.c` — a cross-pool/cross-namespace leak. The per-segment
    /// encode keeps them distinct.
    #[test]
    fn dotted_fields_do_not_collide_across_segments() {
        assert_ne!(
            dispatch_channel_name("a.b", "c"),
            dispatch_channel_name("a", "b.c"),
            "a '.' in a field must not bleed across the segment separator"
        );
    }

    /// More reserved-char shifts that the raw `format!` collapsed but the encode
    /// must keep distinct — the dot can sit on either side of the boundary.
    #[test]
    fn reserved_char_shifts_stay_distinct() {
        // Dot at the end of namespace vs start of task_queue.
        assert_ne!(
            dispatch_channel_name("ns.", "tq"),
            dispatch_channel_name("ns", ".tq")
        );
        // Empty field vs the dot living in the other field.
        assert_ne!(
            dispatch_channel_name("", "a.b"),
            dispatch_channel_name(".a", "b")
        );
        // The escape char itself must not let a literal `%2E` impersonate an
        // encoded `.`: `("%2E", "x")` (literal percent-two-E) must differ from
        // `(".", "x")` (an actual dot, which encodes to `%2E`).
        assert_ne!(
            dispatch_channel_name("%2E", "x"),
            dispatch_channel_name(".", "x")
        );
    }

    /// Encoding is injective in BOTH fields independently and is exactly
    /// reversible (the property the channel relies on), so a small exhaustive
    /// sweep of reserved-char arrangements yields all-distinct channels.
    #[test]
    fn encoding_is_injective_over_reserved_char_pairs() {
        let fields = ["a", "a.b", "a.", ".a", ".", "", "%", "%2E", "a%b", "%2."];
        let mut channels = std::collections::HashSet::new();
        for ns in fields {
            for tq in fields {
                let channel = dispatch_channel_name(ns, tq);
                assert!(
                    channels.insert(channel.clone()),
                    "collision on ({ns:?}, {tq:?}) -> {channel}"
                );
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
    /// `activity_type` does NOT enter the channel.
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
}
