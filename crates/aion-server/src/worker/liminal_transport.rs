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
//! - **Hard-coded addressing.** One liminal server address + one channel name,
//!   provided to [`LiminalOutboxDispatch::new`]. There is no
//!   `(namespace, activity_type)` channel derivation yet — that is 13-3.
//! - **Happy path, one worker.** A single dispatch + result round-trip. Retry
//!   through the honest delivery ack is exercised (the dispatch-out contract)
//!   but the wider retry/backoff/dead-letter proof is 13-1.
//! - **No new outbox schema.** The `dispatch_key` is reused verbatim as the
//!   liminal idempotency key; no `namespace` column is added (that is 13-3).
//!
//! # The two seams it implements
//!
//! - [`LiminalOutboxDispatch`] implements
//!   [`OutboxRowDispatch`](super::outbox_dispatcher::OutboxRowDispatch): it maps
//!   an [`OutboxRow`] to a [`DispatchRequest`] and publishes it over liminal with
//!   the `dispatch_key` as the per-message idempotency key, via
//!   `publish_with_idempotency_key`. It returns `Ok(())` ONLY when the returned
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
//! deferred work tracked by 13-3/13-6 and a corresponding liminal worker-pool
//! seam. This module's types and contracts are written so that swap is a change
//! of responder, not of the aion-side wiring.

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

/// Cross-node [`OutboxRowDispatch`] that places a claimed row over liminal.
///
/// Holds the hard-coded server address + channel name for the spike. Each
/// dispatch opens a fresh [`RemoteChannelHandle`] (one connection, happy path);
/// connection pooling/reuse is a later increment.
pub struct LiminalOutboxDispatch {
    server_address: String,
    channel_name: String,
}

impl std::fmt::Debug for LiminalOutboxDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiminalOutboxDispatch")
            .field("server_address", &self.server_address)
            .field("channel_name", &self.channel_name)
            .finish()
    }
}

impl LiminalOutboxDispatch {
    /// Build a liminal dispatch over a hard-coded server address + channel.
    #[must_use]
    pub fn new(server_address: impl Into<String>, channel_name: impl Into<String>) -> Self {
        Self {
            server_address: server_address.into(),
            channel_name: channel_name.into(),
        }
    }

    /// Connects a remote channel handle to the configured liminal server.
    fn connect(&self) -> Result<RemoteChannelHandle, ServerError> {
        let config = RemoteConfig::new(
            self.server_address.clone(),
            self.channel_name.clone(),
            self.channel_name.clone(),
            SPIKE_POOL,
        )
        .map_err(|error| self.dispatch_error(format!("remote config invalid: {error}")))?;
        let connected = config
            .connect_tcp()
            .map_err(|error| self.dispatch_error(format!("connect failed: {error}")))?;
        RemoteChannelHandle::new(&connected)
            .map_err(|error| self.dispatch_error(format!("handle build failed: {error}")))
    }

    /// Wraps a reason in the existing worker-dispatch error so a non-accepted
    /// send drives the outbox's unchanged retry/backoff/dead-letter path.
    fn dispatch_error(&self, reason: String) -> ServerError {
        ServerError::WorkerDispatch {
            namespace: "liminal".to_owned(),
            activity_type: self.channel_name.clone(),
            reason,
        }
    }
}

#[async_trait]
impl OutboxRowDispatch for LiminalOutboxDispatch {
    async fn dispatch(&self, row: &OutboxRow) -> Result<(), ServerError> {
        let handle = self.connect()?;
        let request = request_for_row(row);
        // Use the dispatch_key as the per-message idempotency key so a
        // re-dispatch (aion retry, reconciler re-arm, crash recovery) reuses the
        // SAME key and liminal's dedup-on-delivery suppresses a second delivery.
        let ack: DeliveryAck = handle
            .publish_with_idempotency_key(&request, &row.dispatch_key)
            .map_err(|error| self.dispatch_error(format!("publish failed: {error}")))?;
        // The load-bearing contract: treat the send as done ONLY on a genuine
        // delivery ack (a worker received it). A non-accept (no subscriber OR a
        // dedup-suppressed duplicate) returns Err so the outbox retries — see
        // the dedup<->retry composition note in the module-level docs and the
        // 13-0 report.
        if ack.is_accepted() {
            Ok(())
        } else {
            Err(self.dispatch_error(
                "liminal delivery ack reported the publish reached no worker (empty channel \
                 or dedup-suppressed duplicate)"
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
