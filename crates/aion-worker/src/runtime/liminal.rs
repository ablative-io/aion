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

use std::sync::Arc;
use std::time::Duration;

use aion_core::{ActivityId, ContentType, Payload, RunId, WorkflowId};
use liminal::protocol::WorkerRegistration;
use liminal_sdk::{PushClient, PushedFrame};
use serde::{Deserialize, Serialize};

use crate::activity::ActivityRegistry;
use crate::config::WorkerConfig;
use crate::context::ActivityContext;
use crate::error::WorkerError;
use crate::protocol::ActivityTask;
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

/// How long the serve loop blocks for the next server push before re-checking the
/// shutdown flag. A bounded poll lets [`LiminalActivityWorker::serve_until`] stop
/// promptly on a quiet connection rather than blocking forever.
const RECV_POLL: Duration = Duration::from_millis(100);

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
        let registration = registration_from(config, &registry);
        let client = PushClient::connect_with_registration(address, registration)
            .map_err(|error| transport_error(&error))?;
        Ok(Self { client, registry })
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

    /// Decodes one pushed frame into a [`DispatchRequest`], executes the activity,
    /// and writes the correlated [`DispatchResponse`] reply.
    async fn handle_pushed_frame(&self, frame: PushedFrame) -> Result<(), WorkerError> {
        let correlation_id = frame.correlation_id();
        let request: DispatchRequest =
            serde_json::from_slice(frame.payload()).map_err(WorkerError::decode)?;
        let response = self.execute(&request).await?;
        let payload = serde_json::to_vec(&response).map_err(WorkerError::encode)?;
        self.client
            .reply(correlation_id, payload)
            .map_err(|error| transport_error(&error))
    }

    /// Executes one dispatch through the typed activity registry, mapping the
    /// outcome onto a [`DispatchResponse`]. A missing handler or a decode failure
    /// becomes a failure outcome (a reason string), never a dropped reply, so the
    /// server always sees a correlated answer it can re-enter.
    async fn execute(&self, request: &DispatchRequest) -> Result<DispatchResponse, WorkerError> {
        let activity_id = ActivityId::from_sequence_position(request.ordinal);
        let input = Payload::new(ContentType::Json, request.input.clone());
        // The dispatch attempt is not carried on the push wire (it lives on the
        // server's outbox row); the worker stamps a 1-based attempt for the
        // execution context, exactly as a first delivery would.
        let task = ActivityTask {
            workflow_id: request.workflow_id.clone(),
            activity_id: activity_id.clone(),
            run_id: request.run_id.clone(),
            activity_type: request.activity_type.clone(),
            attempt: 1,
            input,
            labels: std::collections::BTreeMap::new(),
        };
        let (context, cancellation) = ActivityContext::new(activity_id, task.attempt);
        // The push transport has no cooperative-cancellation channel in the spike;
        // drop the handle so the activity simply runs to completion.
        drop(cancellation);

        let outcome = match self.registry.dispatch(task, context).await {
            Ok(outcome) => outcome,
            Err(error) => {
                // A registry-level error (no handler, encode failure) is reported
                // back as a failure outcome rather than a dropped dispatch.
                return Ok(DispatchResponse {
                    workflow_id: request.workflow_id.clone(),
                    ordinal: request.ordinal,
                    run_id: request.run_id.clone(),
                    outcome: Err(error.to_string()),
                });
            }
        };

        let outcome = match outcome {
            DispatchOutcome::Completed { output } => Ok(result_string(&output)),
            DispatchOutcome::Failed { failure } => Err(failure.message),
        };
        Ok(DispatchResponse {
            workflow_id: request.workflow_id.clone(),
            ordinal: request.ordinal,
            run_id: request.run_id.clone(),
            outcome,
        })
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
