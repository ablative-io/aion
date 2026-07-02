//! [`NornSession`] — a live Norn `--protocol jsonrpc` run behind the [`AgentSession`] seam.
//!
//! One background **pump task** owns the connection's single reader and demultiplexes every
//! inbound frame off the shared JSON-RPC channel:
//!
//! - a `event/*` **notification** is translated ([`crate::translate::notification_to_event`]) and
//!   forwarded to the [`AgentSession::events`] stream — it has no `id`, so it can NEVER be
//!   captured as the result,
//! - a **Response** is routed by its `id` to the one waiter that sent that request — the
//!   `run/execute` waiter (the terminal result) or an `intervene/*` waiter (its ack),
//! - a child-initiated **Request** (e.g. a future `approval/*`) is outside NOI-4 scope and is
//!   traced and dropped rather than answered.
//!
//! This id-routing is what makes the result/event split structural (§4.1): the terminal result is
//! captured ONLY as the Response whose `id` matches the `run/execute` request the harness sent.

use std::collections::HashMap;
use std::sync::Arc;

use aion_core::{ActivityEvent, InterventionCapabilities, InterventionCommand, Payload};
use aion_integrations::contract::AgentSession;
use aion_integrations::jsonrpc::{
    IncomingMessage, JsonRpcConnection, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
};
use aion_integrations::{ContentType, HarnessError};
use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::translate::{self, EventIdentity};

/// The set of request-id → response-waiter registrations the pump routes Responses to.
type Waiters = Arc<Mutex<HashMap<JsonRpcId, oneshot::Sender<JsonRpcResponse>>>>;

/// A live Norn run for one activity attempt.
///
/// Generic over the child's read/write halves so tests can drive it over an in-memory duplex; in
/// production the halves are the child process's stdout/stdin.
pub struct NornSession<R, W> {
    connection: Arc<JsonRpcConnection<R, W>>,
    capabilities: InterventionCapabilities,
    events: Option<mpsc::UnboundedReceiver<ActivityEvent>>,
    waiters: Waiters,
    run_id: JsonRpcId,
    run_result: Option<oneshot::Receiver<JsonRpcResponse>>,
    pump: Option<tokio::task::JoinHandle<()>>,
    /// The spawned child, reaped when the session is dropped. `None` for the in-memory test path.
    child: Option<tokio::process::Child>,
}

impl<R, W> NornSession<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    /// Builds a session over an already-handshaked connection and starts the reader pump.
    ///
    /// `run_id` is the id of the outstanding `run/execute` request whose Response is the result.
    /// `identity` stamps the run key onto every translated event. The pump is started here so
    /// events stream from the moment the session exists.
    pub(crate) fn start(
        connection: Arc<JsonRpcConnection<R, W>>,
        capabilities: InterventionCapabilities,
        run_id: JsonRpcId,
        identity: EventIdentity,
        child: Option<tokio::process::Child>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (result_tx, result_rx) = oneshot::channel();

        // Register the run/execute waiter into the map BEFORE the pump can observe its Response, by
        // building the map with the entry already present — no lock, no `.await` in this sync ctor.
        let mut initial = HashMap::new();
        initial.insert(run_id.clone(), result_tx);
        let waiters: Waiters = Arc::new(Mutex::new(initial));

        let pump = tokio::spawn(pump_loop(
            Arc::clone(&connection),
            Arc::clone(&waiters),
            event_tx,
            identity,
        ));

        Self {
            connection,
            capabilities,
            events: Some(event_rx),
            waiters,
            run_id,
            run_result: Some(result_rx),
            pump: Some(pump),
            child,
        }
    }
}

/// The reader pump: demultiplexes every inbound frame until end-of-stream.
///
/// Notifications become events; Responses are routed to their id's waiter; child-initiated
/// Requests are traced and dropped (out of NOI-4 scope). On a transport error or EOF the loop
/// ends, the event channel closes (ending the events stream), and any un-answered waiter observes
/// a dropped sender (a `RecvError` its awaiter maps to a transport error).
async fn pump_loop<R, W>(
    connection: Arc<JsonRpcConnection<R, W>>,
    waiters: Waiters,
    event_tx: mpsc::UnboundedSender<ActivityEvent>,
    identity: EventIdentity,
) where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut worker_seq: u64 = 0;
    loop {
        match connection.recv().await {
            Ok(Some(IncomingMessage::Notification(note))) => {
                let params = note.params.unwrap_or(serde_json::Value::Null);
                let event =
                    translate::notification_to_event(&identity, worker_seq, &note.method, &params);
                worker_seq = worker_seq.saturating_add(1);
                // A send failure means the consumer dropped the events stream; keep pumping so the
                // result and acks still route, just stop forwarding events. Not an error.
                let _ = event_tx.send(event);
            }
            Ok(Some(IncomingMessage::Response(response))) => {
                if let Some(sender) = waiters.lock().await.remove(&response.id) {
                    // A dropped receiver (the waiter gave up) is not an error worth surfacing.
                    let _ = sender.send(response);
                }
            }
            Ok(Some(IncomingMessage::Request(request))) => {
                tracing::debug!(
                    method = %request.method,
                    "norn adapter: ignoring child-initiated request (out of NOI-4 scope)",
                );
            }
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error, "norn adapter: reader pump ended on transport error");
                return;
            }
        }
    }
}

#[async_trait]
impl<R, W> AgentSession for NornSession<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    fn capabilities(&self) -> &InterventionCapabilities {
        &self.capabilities
    }

    fn events(&mut self) -> BoxStream<'static, ActivityEvent> {
        // The stream is taken once; a second call yields an empty stream rather than panicking.
        match self.events.take() {
            Some(receiver) => Box::pin(futures::stream::unfold(
                receiver,
                |mut receiver| async move { receiver.recv().await.map(|event| (event, receiver)) },
            )),
            None => Box::pin(futures::stream::empty()),
        }
    }

    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError> {
        // Capability-gate FIRST: an unadvertised primitive is rejected before any request is sent,
        // and the three unsupported primitives never even build a Norn request.
        if !self.capabilities.supports(&cmd.kind) {
            return Err(HarnessError::capability_not_supported(format!(
                "{:?}",
                cmd.kind.primitive()
            )));
        }
        let request = translate::intervention_to_request(&cmd.kind)?;

        let id = self.connection.next_request_id();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.waiters.lock().await.insert(id.clone(), ack_tx);

        let frame = JsonRpcRequest::new(id.clone(), request.method, Some(request.params));
        if let Err(error) = self.connection.send_request(&frame).await {
            self.waiters.lock().await.remove(&id);
            return Err(error);
        }

        let response = ack_rx.await.map_err(|_recv| {
            HarnessError::transport("intervene ack channel closed before reply")
        })?;
        response_into_ack(response)
    }

    async fn wait_result(mut self) -> Result<Payload, HarnessError> {
        let receiver = self
            .run_result
            .take()
            .ok_or_else(|| HarnessError::protocol("run result already awaited"))?;
        let response = receiver.await.map_err(|_recv| {
            HarnessError::transport("run/execute response channel closed before reply")
        })?;
        debug_assert_eq!(
            response.id, self.run_id,
            "pump routed the run/execute response"
        );
        response_into_payload(response)
    }
}

/// Interprets an `intervene/*` Response as an ack: success is `Ok(())`, an error object surfaces.
fn response_into_ack(response: JsonRpcResponse) -> Result<(), HarnessError> {
    if let Some(error) = response.error {
        return Err(HarnessError::harness(format!(
            "intervene rejected (code {}): {}",
            error.code, error.message
        )));
    }
    Ok(())
}

/// Interprets a `run/execute` Response as the terminal [`Payload`].
///
/// The success `result` is Norn's versioned stop envelope (`envelope_version: 1`), interpreted by
/// [`translate::run_result_to_output`]:
///
/// - `stop.reason == "completed"` → the neutral [`Payload`] is the envelope's `output` VALUE
///   serialized as JSON (a JSON string for schema-less runs; the JSON object itself when Norn ran
///   with an output schema — never stringified twice),
/// - any other `stop.reason` → [`HarnessError::Harness`] carrying the whole `stop` object
///   (reason plus per-variant detail) verbatim — and the partial `output`, when the envelope
///   carried one — so the caller can judge retry (or accept the partial),
/// - a `null` result (a prompt that resolved entirely to a local slash command) →
///   [`HarnessError::Harness`] — an agent activity must produce output,
/// - a result-less, error-less Response (a broken peer) → [`HarnessError::Protocol`],
/// - a non-envelope result → [`HarnessError::Protocol`] naming what was missing.
///
/// An error object on the Response surfaces as [`HarnessError::Harness`].
fn response_into_payload(response: JsonRpcResponse) -> Result<Payload, HarnessError> {
    if let Some(error) = response.error {
        return Err(HarnessError::harness(format!(
            "run/execute failed (code {}): {}",
            error.code, error.message
        )));
    }
    // The envelope type keeps key presence: `None` means the frame carried NO `result` key — with
    // `error` also absent that is a broken peer, not a payload. A PRESENT `"result": null`
    // (`Some(Value::Null)`) is the contract's answer for a prompt that resolved entirely to a
    // local slash command.
    let result = match response.result {
        None => {
            return Err(HarnessError::protocol(
                "run/execute response carried neither result nor error",
            ));
        }
        Some(serde_json::Value::Null) => {
            return Err(HarnessError::harness(
                "run resolved to a local slash command; no output",
            ));
        }
        Some(result) => result,
    };
    let output = translate::run_result_to_output(&result)?;
    let bytes = serde_json::to_vec(&output).map_err(|source| {
        HarnessError::protocol(format!("run output is not encodable: {source}"))
    })?;
    Ok(Payload::new(ContentType::Json, bytes))
}

impl<R, W> Drop for NornSession<R, W> {
    fn drop(&mut self) {
        // Abort the pump and reap the child so a dropped session never leaks a task or a process.
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
        if let Some(mut child) = self.child.take() {
            // A best-effort kill; the child may already have exited.
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    //! Fast unit tests of the session against an IN-TEST FAKE PEER over an in-memory duplex — no
    //! real process. The fake peer feeds canned JSON-RPC frames (notifications + the id-matched
    //! run/execute Response + intervene acks) using the same `aion-integrations` jsonrpc helper the
    //! adapter itself uses, so the pump's demux is exercised end-to-end without spawning `norn`.

    use super::*;
    use aion_core::{
        ActivityEventKind, ActivityId, InjectPriority, InterventionCapabilities, InterventionKind,
        InterventionPrimitive, WorkflowId,
    };
    use aion_integrations::jsonrpc::{JsonRpcNotification, JsonRpcResponse};
    use chrono::Utc;
    use futures::StreamExt;
    use serde_json::json;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf, duplex, split};
    use uuid::Uuid;

    type SessionUnderTest = NornSession<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;
    /// The fake peer's own framed connection: it writes notifications/responses the session reads,
    /// and reads the session's outbound intervene requests.
    type PeerConnection = JsonRpcConnection<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

    fn identity() -> EventIdentity {
        EventIdentity {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(2),
            attempt: 1,
        }
    }

    fn caps() -> InterventionCapabilities {
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ])
    }

    /// Wire a session to a fake peer over a loopback duplex. The session reads what the peer writes
    /// and vice versa. The run/execute id is fixed so the peer can id-match the result Response.
    fn wire() -> (SessionUnderTest, PeerConnection, JsonRpcId) {
        let (session_io, peer_io) = duplex(8192);
        let (s_read, s_write) = split(session_io);
        let (p_read, p_write) = split(peer_io);
        let connection = Arc::new(JsonRpcConnection::new(s_read, s_write));
        let run_id = JsonRpcId::number(1);
        let session = NornSession::start(connection, caps(), run_id.clone(), identity(), None);
        let peer = JsonRpcConnection::new(p_read, p_write);
        (session, peer, run_id)
    }

    fn text_note(text: &str) -> JsonRpcNotification {
        JsonRpcNotification::new(
            "event/message",
            Some(json!({
                "type": "text",
                "text": text,
                "agent_id": Uuid::nil().to_string(),
                "agent_role": "root",
            })),
        )
    }

    /// A `run/execute` success result in the versioned stop-envelope shape a completed run
    /// carries (the fields beyond `envelope_version`/`stop`/`output` are realistic passengers).
    fn completed_envelope(output: &serde_json::Value) -> serde_json::Value {
        json!({
            "envelope_version": 1,
            "stop": { "reason": "completed" },
            "output": output,
            "usage": { "input_tokens": 12, "output_tokens": 3 },
            "model": "mock-model",
            "session_id": "step-042",
            "events": [],
            "diagnostics": [],
        })
    }

    /// A non-completed stop envelope with the given internally-tagged `stop` object and `output`
    /// (`null` for the reasons that carry none; the partial for those that may).
    fn stopped_envelope(stop: &serde_json::Value, output: &serde_json::Value) -> serde_json::Value {
        json!({
            "envelope_version": 1,
            "stop": stop,
            "output": output,
            "model": "mock-model",
            "session_id": "step-042",
        })
    }

    /// Runs [`response_into_payload`] on a success Response carrying `result`.
    fn payload_of(result: serde_json::Value) -> Result<Payload, HarnessError> {
        response_into_payload(JsonRpcResponse::success(JsonRpcId::number(1), result))
    }

    fn inject_command() -> InterventionCommand {
        InterventionCommand {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(2),
            attempt: 1,
            issued_by: Some("operator".to_owned()),
            issued_at: Utc::now(),
            kind: InterventionKind::InjectMessage {
                text: "steer now".to_owned(),
                priority: InjectPriority::Interrupt,
            },
        }
    }

    #[tokio::test]
    async fn capabilities_are_the_negotiated_set() {
        let (session, _peer, _id) = wire();
        assert!(
            session
                .capabilities()
                .supports_primitive(InterventionPrimitive::InjectMessage)
        );
        assert!(
            session
                .capabilities()
                .supports_primitive(InterventionPrimitive::Cancel)
        );
    }

    #[tokio::test]
    async fn live_events_stream_from_peer_notifications() {
        let (mut session, peer, run_id) = wire();
        let mut events = session.events();

        // The peer streams two live event/* notifications, then the id-matched result.
        peer.send_notification(&text_note("working")).await.unwrap();
        peer.send_notification(&text_note("still working"))
            .await
            .unwrap();

        let first = events.next().await.expect("first event streams live");
        assert_eq!(first.attempt, 1);
        match first.kind {
            ActivityEventKind::Message { text, .. } => assert_eq!(text, "working"),
            other => panic!("expected Message, got {other:?}"),
        }
        let second = events.next().await.expect("second event streams live");
        match second.kind {
            ActivityEventKind::Message { text, .. } => assert_eq!(text, "still working"),
            other => panic!("expected Message, got {other:?}"),
        }

        // Close the run so wait_result completes and the stream ends.
        peer.send_response(&JsonRpcResponse::success(
            run_id,
            completed_envelope(&json!("all done")),
        ))
        .await
        .unwrap();
        let payload = session.wait_result().await.unwrap();
        assert_eq!(payload.content_type(), &ContentType::Json);
    }

    #[tokio::test]
    async fn wait_result_returns_the_id_matched_run_execute_response_payload() {
        let (session, peer, run_id) = wire();
        // A stray notification and a non-matching Response must NOT be captured as the result.
        peer.send_notification(&text_note("noise")).await.unwrap();
        peer.send_response(&JsonRpcResponse::success(
            JsonRpcId::number(999),
            json!({ "not": "the result" }),
        ))
        .await
        .unwrap();
        // Only the id-matched run/execute Response is the result.
        peer.send_response(&JsonRpcResponse::success(
            run_id,
            completed_envelope(&json!({ "answer": 7 })),
        ))
        .await
        .unwrap();

        let payload = session.wait_result().await.unwrap();
        let decoded = payload.to_json().unwrap();
        assert_eq!(decoded, json!({ "answer": 7 }));
    }

    #[tokio::test]
    async fn intervene_sends_request_and_awaits_the_peer_ack() {
        let (session, peer, _run_id) = wire();

        // Drive intervene concurrently: the peer must read the request and ack it.
        let intervene = tokio::spawn(async move { session.intervene(inject_command()).await });

        // The peer reads the outbound intervene/injectMessage request and acks its id.
        let message = peer
            .recv()
            .await
            .unwrap()
            .expect("an intervene request arrives");
        let request = match message {
            IncomingMessage::Request(request) => request,
            other => panic!("expected a request, got {other:?}"),
        };
        assert_eq!(request.method, "intervene/injectMessage");
        assert_eq!(request.params.as_ref().unwrap()["text"], json!("steer now"));
        assert_eq!(
            request.params.as_ref().unwrap()["priority"],
            json!("interrupt")
        );
        peer.send_response(&JsonRpcResponse::success(
            request.id,
            json!({ "status": "injected" }),
        ))
        .await
        .unwrap();

        intervene
            .await
            .unwrap()
            .expect("the ack resolves the intervene");
    }

    #[tokio::test]
    async fn intervene_rejects_an_unsupported_primitive_without_sending() {
        let (session, peer, _run_id) = wire();
        let mut cmd = inject_command();
        cmd.kind = InterventionKind::PauseResume { paused: true };

        let error = session.intervene(cmd).await.unwrap_err();
        assert!(
            matches!(error, HarnessError::CapabilityNotSupported { .. }),
            "an unadvertised primitive is capability-gated, got {error:?}"
        );
        // Nothing was sent: the peer sees no frame before EOF (drop the session to close it).
        drop(session);
        let next = peer.recv().await.unwrap();
        assert!(
            next.is_none(),
            "no request must reach the peer for a gated primitive"
        );
    }

    #[tokio::test]
    async fn intervene_surfaces_a_peer_error_ack() {
        let (session, peer, _run_id) = wire();
        let intervene = tokio::spawn(async move { session.intervene(inject_command()).await });

        let message = peer.recv().await.unwrap().unwrap();
        let request = match message {
            IncomingMessage::Request(request) => request,
            other => panic!("expected a request, got {other:?}"),
        };
        peer.send_response(&JsonRpcResponse::failure(
            request.id,
            aion_integrations::jsonrpc::JsonRpcError::new(-32603, "inbound channel full"),
        ))
        .await
        .unwrap();

        let error = intervene.await.unwrap().unwrap_err();
        assert!(
            matches!(error, HarnessError::Harness { .. }),
            "a rejected intervene surfaces as a harness error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_notification_is_never_captured_as_the_result() {
        // The negative control: even an event/* notification whose params look result-shaped can
        // never fill the result slot, because it carries no id.
        let (session, peer, run_id) = wire();
        peer.send_notification(&JsonRpcNotification::new(
            "event/stop",
            Some(json!({
                "type": "done", "stop_reason": "end_turn",
                "agent_id": Uuid::nil().to_string(), "agent_role": "root",
            })),
        ))
        .await
        .unwrap();
        // Now the real result.
        peer.send_response(&JsonRpcResponse::success(
            run_id,
            completed_envelope(&json!("the real result")),
        ))
        .await
        .unwrap();
        let payload = session.wait_result().await.unwrap();
        assert_eq!(payload.to_json().unwrap(), json!("the real result"));
    }

    // --- response_into_payload: the stop-envelope branches ---

    #[test]
    fn completed_string_output_is_the_json_string_payload() {
        // A schema-less run's output is a string: the payload is that VALUE serialized as JSON —
        // one layer of quoting, never stringified twice.
        let payload = payload_of(completed_envelope(&json!("the final answer"))).unwrap();
        assert_eq!(payload.content_type(), &ContentType::Json);
        assert_eq!(payload.to_json().unwrap(), json!("the final answer"));
    }

    #[test]
    fn completed_structured_output_passes_through_as_the_object() {
        // With an output schema, `output` is the validated JSON object: it passes through as-is.
        let output = json!({ "verdict": "pass", "notes": ["a", "b"] });
        let payload = payload_of(completed_envelope(&output)).unwrap();
        assert_eq!(payload.to_json().unwrap(), output);
    }

    #[test]
    fn each_non_completed_stop_reason_surfaces_with_its_detail_and_partial_verbatim() {
        // Every non-completed reason is a harness error whose message carries the reason AND its
        // per-variant detail fields — the caller judges retry on that text. The reasons that may
        // hold a partial `output` (timed_out / truncated / schema_unreachable) must carry it in
        // the same message, labelled, so an accept-the-partial policy stays reachable.
        let cases: Vec<(serde_json::Value, serde_json::Value, Vec<&str>)> = vec![
            (
                json!({ "reason": "schema_unreachable", "attempts": 3,
                        "validation_errors": ["missing field `verdict`"] }),
                json!({ "notes": "best attempt" }),
                vec![
                    "schema_unreachable",
                    "attempts",
                    "3",
                    "validation_errors",
                    "missing field `verdict`",
                    "partial output: {\"notes\":\"best attempt\"}",
                ],
            ),
            (
                json!({ "reason": "max_iterations" }),
                json!(null),
                vec!["max_iterations"],
            ),
            (
                json!({ "reason": "timed_out", "elapsed_ms": 300_000, "iterations": 12 }),
                json!("a half-written summary"),
                vec![
                    "timed_out",
                    "elapsed_ms",
                    "300000",
                    "iterations",
                    "12",
                    "partial output: \"a half-written summary\"",
                ],
            ),
            (
                json!({ "reason": "cancelled" }),
                json!(null),
                vec!["cancelled"],
            ),
            (
                json!({ "reason": "truncated", "truncation": "max_tokens", "iterations": 2 }),
                json!("cut off mid-sente"),
                vec![
                    "truncated",
                    "truncation",
                    "max_tokens",
                    "iterations",
                    "2",
                    "partial output: \"cut off mid-sente\"",
                ],
            ),
        ];
        for (stop, output, fragments) in cases {
            let error = payload_of(stopped_envelope(&stop, &output)).unwrap_err();
            assert!(
                matches!(error, HarnessError::Harness { .. }),
                "a non-completed stop is a harness error, got {error:?}"
            );
            let message = error.to_string();
            for fragment in fragments {
                assert!(
                    message.contains(fragment),
                    "stop {stop} must surface `{fragment}` verbatim, got: {message}"
                );
            }
            if output.is_null() {
                assert!(
                    !message.contains("partial output"),
                    "a null output must not fabricate a partial: {message}"
                );
            }
        }
    }

    #[test]
    fn null_result_is_the_slash_command_harness_error() {
        // A prompt that resolves entirely to a local slash command is answered with a success
        // Response whose result is JSON null — an agent activity must produce output.
        let response: JsonRpcResponse =
            serde_json::from_value(json!({ "jsonrpc": "2.0", "id": 1, "result": null })).unwrap();
        let error = response_into_payload(response).unwrap_err();
        assert!(
            matches!(error, HarnessError::Harness { .. }),
            "a null result is a harness error, got {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "harness reported failure: run resolved to a local slash command; no output"
        );
    }

    #[test]
    fn a_response_with_neither_result_nor_error_is_a_protocol_error() {
        // A frame with NO `result` key and no `error` is a broken peer — distinct from the legal
        // `"result": null` slash-command answer above.
        let response: JsonRpcResponse =
            serde_json::from_value(json!({ "jsonrpc": "2.0", "id": 1 })).unwrap();
        let error = response_into_payload(response).unwrap_err();
        assert!(
            matches!(error, HarnessError::Protocol { .. }),
            "a result-less, error-less response is a protocol error, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("carried neither result nor error"),
            "the error names the broken frame: {error}"
        );
    }

    #[test]
    fn completed_envelope_with_present_null_output_passes_null_through() {
        // `"output": null` on a completed envelope is a legal null output — the payload is JSON
        // null, never an error.
        let payload = payload_of(completed_envelope(&json!(null))).unwrap();
        assert_eq!(payload.to_json().unwrap(), json!(null));
    }

    #[test]
    fn completed_envelope_missing_the_output_key_is_a_protocol_error() {
        // The contract says completed ALWAYS carries `output`; an absent key is off-contract.
        let error = payload_of(json!({
            "envelope_version": 1,
            "stop": { "reason": "completed" },
        }))
        .unwrap_err();
        assert!(
            matches!(error, HarnessError::Protocol { .. }),
            "a completed envelope without output is a protocol error, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("completed envelope carried no output field")
        );
    }

    #[test]
    fn non_envelope_result_is_a_protocol_error_never_a_silent_passthrough() {
        // The pre-envelope result shape must be rejected naming what was missing.
        let error = payload_of(json!({ "result": "completed", "output": "x" })).unwrap_err();
        assert!(
            matches!(error, HarnessError::Protocol { .. }),
            "an unknown result shape is a protocol error, got {error:?}"
        );
        assert!(error.to_string().contains("envelope_version"));

        let error = payload_of(json!({ "envelope_version": 1, "output": "x" })).unwrap_err();
        assert!(matches!(error, HarnessError::Protocol { .. }));
        assert!(error.to_string().contains("`stop`"));
    }
}
