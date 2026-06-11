//! WebSocket forward loop + lag handling.

use std::num::NonZeroUsize;

use aion_core::{Event, WorkflowId};
use aion_proto::SubscriptionRequest;
use aion_proto::{WireError, WireErrorCode, encode_streamed_event};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};

use crate::config::EVENT_BROADCAST_CAPACITY_REQUIRED;
use crate::error::ServerError;
use crate::namespace::CallerIdentity;
use crate::state::ServerState;
use crate::stream::namespace_filter::{GateVerdict, NamespaceEventGate};
use crate::stream::selector::SubscriptionSelector;
use crate::stream::subscribe::{EventSubscription, subscribe_events};

/// Encoded event frame queued for a WebSocket connection.
pub type EncodedFrame = String;

/// `error_type` discriminator for the per-workflow contiguity tripwire: a
/// delivered-stream sequence gap or regression that should be unreachable
/// under the splice invariants, surfaced loudly instead of delivering a
/// gapped stream.
pub const SEQUENCE_CONTIGUITY_VIOLATION: &str = "SequenceContiguityViolation";

/// Authorize a wire subscription request and forward it on an accepted socket.
///
/// The per-connection buffer bound and the namespace-gate verdict-cache bound
/// are read from runtime config, not defaulted in the transport loop.
///
/// A subscription rejected before streaming (namespace authorization failure,
/// per-workflow target failure, or resume-cursor validation failure) is never
/// a silent drop: the rejection is sent to the client as one terminal
/// `{"error": <WireError>}` frame followed by a close frame, so SDKs can
/// branch on the stable code instead of reconnecting against a deterministic
/// denial.
///
/// # Errors
///
/// Returns [`ServerError`] when namespace authorization, engine subscription,
/// frame serialization, or bounded-buffer forwarding fails.
pub async fn handle_subscription_socket(
    mut socket: WebSocket,
    state: &ServerState,
    caller: &CallerIdentity,
    request: &SubscriptionRequest,
) -> Result<(), ServerError> {
    let subscription = match subscribe_events(state.namespace_guard(), caller, request).await {
        Ok(subscription) => subscription,
        Err(error) => {
            send_wire_error(&mut socket, &error.to_wire_error()).await?;
            return Err(error);
        }
    };
    // The gate's per-workflow verdict cache is bounded by the configured
    // broadcast capacity: the engine-global channel retains at most that many
    // events, so any burst this connection can observe without lagging out
    // references at most that many distinct workflows. Startup validation
    // requires the value, so absence here is a wiring bug reported loudly.
    let Some(gate_capacity) = state
        .runtime_config()
        .websocket
        .event_broadcast_capacity
        .and_then(NonZeroUsize::new)
    else {
        let error = ServerError::Config {
            message: EVENT_BROADCAST_CAPACITY_REQUIRED.to_owned(),
        };
        send_wire_error(&mut socket, &error.to_wire_error()).await?;
        return Err(error);
    };
    // The broadcast channel is engine-global with no namespace dimension, so
    // every delivered event passes the namespace gate before encoding. The
    // guard-verified per-workflow target is pre-seeded as allowed.
    let mut gate = NamespaceEventGate::new(
        state.namespace_guard().resolver().clone(),
        subscription.namespace.clone(),
        gate_capacity,
    );
    if let Some(target) = &subscription.workflow_target {
        gate.allow(target.clone());
    }
    let outbound_buffer_bound = state.runtime_config().websocket.outbound_buffer_bound;
    forward_subscription(socket, subscription, gate, outbound_buffer_bound).await
}

/// Forward a previously authorized engine subscription to a WebSocket.
///
/// The reader task is aborted on every exit path — success, terminal error
/// frame, and frame-encoding failure alike — so it can never linger holding a
/// broadcast receiver after the socket loop ends.
///
/// # Errors
///
/// Returns [`ServerError`] when the stream ends with a terminal error frame
/// (lag, gate failure, encoding failure) or a wire frame cannot be encoded.
pub async fn forward_subscription(
    socket: WebSocket,
    subscription: EventSubscription,
    gate: NamespaceEventGate,
    outbound_buffer_bound: usize,
) -> Result<(), ServerError> {
    let EncodedEventStream {
        mut frames,
        lagged,
        reader_done,
    } = spawn_encoded_event_stream(subscription, gate, outbound_buffer_bound)?;
    let (mut socket_tx, mut socket_rx) = socket.split();
    let result = drive_socket(&mut socket_tx, &mut socket_rx, &mut frames, lagged).await;
    // Abort unconditionally, before any error propagation, so the reader can
    // never outlive the connection holding a broadcast receiver.
    reader_done.abort();
    result
}

/// Drive one subscription socket: forward frames, watch for client close, and
/// deliver the terminal error frame deterministically.
///
/// The reader task upholds two ordering guarantees this loop relies on:
/// every frame is queued into the bounded channel *before* the terminal
/// oneshot fires, and the oneshot fires *before* the frame sender is dropped.
/// Whichever `select!` branch wins a race between "frames closed" and
/// "terminal error fired", the client therefore observes the same sequence:
/// all queued event frames, then exactly one terminal error frame, then close.
async fn drive_socket<Tx, Rx>(
    socket_tx: &mut Tx,
    socket_rx: &mut Rx,
    frames: &mut mpsc::Receiver<EncodedFrame>,
    lagged: oneshot::Receiver<WireError>,
) -> Result<(), ServerError>
where
    Tx: futures::Sink<Message> + Unpin,
    <Tx as futures::Sink<Message>>::Error: std::fmt::Debug,
    Rx: futures::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let mut lagged = lagged;
    let mut lag_closed = false;
    loop {
        tokio::select! {
            client_message = socket_rx.next() => {
                match client_message {
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(message)) => drop(message),
                    Some(Err(error)) => {
                        drop(error);
                        return Ok(());
                    }
                }
            }
            lag = &mut lagged, if !lag_closed => {
                match lag {
                    Ok(error) => {
                        // The reader stopped after queueing everything that
                        // will ever exist: deliver the quiescent backlog, then
                        // the terminal error frame — buffered events are never
                        // dropped because the lag branch won the race.
                        return drain_then_terminal(socket_tx, frames, error).await;
                    }
                    Err(_closed) => {
                        lag_closed = true;
                    }
                }
            }
            frame = frames.recv() => {
                let Some(frame) = frame else {
                    // The reader fires the terminal oneshot strictly before
                    // dropping the frame sender, so if a terminal error raced
                    // this branch it is observable now — the client must not
                    // get an abrupt close instead of its error frame.
                    if !lag_closed {
                        if let Ok(error) = lagged.try_recv() {
                            return deliver_terminal(socket_tx, error).await;
                        }
                    }
                    return Ok(());
                };
                if socket_tx.send(Message::Text(frame.into())).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

/// Deliver the reader's already-queued frames, then the terminal error frame.
///
/// Called only after the terminal oneshot fired: the reader queues frames and
/// fires the oneshot from one task in program order, so by the time the value
/// is observed the channel holds every frame that will ever be sent and
/// `try_recv` drains it completely.
async fn drain_then_terminal<Tx>(
    socket_tx: &mut Tx,
    frames: &mut mpsc::Receiver<EncodedFrame>,
    error: WireError,
) -> Result<(), ServerError>
where
    Tx: futures::Sink<Message> + Unpin,
    <Tx as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    while let Ok(frame) = frames.try_recv() {
        if socket_tx.send(Message::Text(frame.into())).await.is_err() {
            // The client is gone; there is no one left to tell.
            return Ok(());
        }
    }
    deliver_terminal(socket_tx, error).await
}

/// Send the terminal error frame + close, then surface the failure typed.
async fn deliver_terminal<Tx>(socket_tx: &mut Tx, error: WireError) -> Result<(), ServerError>
where
    Tx: futures::Sink<Message> + Unpin,
    <Tx as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    send_wire_error(socket_tx, &error).await?;
    Err(ServerError::Wire { wire: error })
}

/// Send one terminal WebSocket error frame followed by a close frame.
///
/// Every WebSocket error frame is the standardized wrapper object
/// `{"error": <WireError as JSON>}` — the shape every SDK detects as a
/// terminal stream error — never a bare `WireError`.
pub(crate) async fn send_wire_error<S>(
    socket_tx: &mut S,
    error: &WireError,
) -> Result<(), ServerError>
where
    S: futures::Sink<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    let frame = serde_json::json!({ "error": error });
    let payload = serde_json::to_string(&frame).map_err(|source| ServerError::Wire {
        wire: WireError::backend(format!("failed to serialize stream error: {source}")),
    })?;
    if socket_tx.send(Message::Text(payload.into())).await.is_err() {
        return Ok(());
    }
    let close = CloseFrame {
        code: close_code::ERROR,
        reason: error.code.as_str().into(),
    };
    let close_result = socket_tx.send(Message::Close(Some(close))).await;
    drop(close_result);
    Ok(())
}

/// Bounded encoded stream built from an engine subscription.
pub struct EncodedEventStream {
    /// Frames ready to write to a socket.
    pub frames: mpsc::Receiver<EncodedFrame>,
    /// Receives a typed terminal error if the bounded frame queue fills, an
    /// engine-side lag item arrives, encoding fails, the namespace gate's
    /// ownership source fails, or the per-workflow contiguity tripwire fires.
    pub lagged: oneshot::Receiver<WireError>,
    /// Reader task owning the upstream engine stream.
    pub reader_done: tokio::task::JoinHandle<()>,
}

/// Spawn the non-blocking engine-reader side of a WebSocket subscription.
///
/// Replay frames (the resume history slice) are queued with awaiting sends so
/// a replay longer than the per-connection buffer is delivered completely —
/// replay can never be silently dropped or spuriously lagged. The live tail
/// keeps `try_send`: the reader never awaits socket capacity for live events,
/// so a slow consumer lags out (one terminal frame) instead of back-pressuring
/// the engine event tail.
///
/// Every event — replay and live, all subscription kinds — passes the
/// namespace gate and the subscription selector before its frame is encoded,
/// and per-workflow streams carry a delivered-sequence contiguity tripwire.
///
/// # Errors
///
/// Returns `ServerError::Config` if `outbound_buffer_bound` is zero.
pub fn spawn_encoded_event_stream(
    subscription: EventSubscription,
    gate: NamespaceEventGate,
    outbound_buffer_bound: usize,
) -> Result<EncodedEventStream, ServerError> {
    if outbound_buffer_bound == 0 {
        return Err(ServerError::Config {
            message: "websocket.outbound_buffer_bound must be greater than zero".to_owned(),
        });
    }

    let EventSubscription {
        namespace,
        workflow_target,
        replay,
        events,
        selector,
        filter: _,
    } = subscription;
    let (frames_tx, frames) = mpsc::channel(outbound_buffer_bound);
    let (lag_tx, lagged) = oneshot::channel();
    let reader = SubscriptionReader {
        namespace,
        workflow_target,
        gate,
        selector,
        contiguity: ContiguityGuard::new(),
        error_tx: Some(lag_tx),
        frames_tx,
    };
    let reader_done = tokio::spawn(reader.run(replay, events));

    Ok(EncodedEventStream {
        frames,
        lagged,
        reader_done,
    })
}

/// Queueing discipline for one frame.
enum QueueMode {
    /// Await channel capacity (replay delivery).
    Awaiting,
    /// `try_send`; a full buffer is a terminal lag (live delivery).
    Bounded,
}

/// Outcome of queuing one frame from the reader task.
enum FrameOutcome {
    /// Frame queued for delivery.
    Delivered,
    /// Event filtered out (namespace gate or subscription selector).
    Filtered,
    /// Receiver gone: stop reading.
    Stop,
}

/// Whether the reader continues after processing one event.
enum ReaderStep {
    /// Keep reading.
    Continue,
    /// Stop: terminal event delivered, terminal error reported, or the
    /// receiver is gone.
    Stop,
}

/// Reader-task state: gates, selects, encodes, and queues events.
struct SubscriptionReader {
    namespace: String,
    workflow_target: Option<WorkflowId>,
    gate: NamespaceEventGate,
    selector: SubscriptionSelector,
    contiguity: ContiguityGuard,
    error_tx: Option<oneshot::Sender<WireError>>,
    frames_tx: mpsc::Sender<EncodedFrame>,
}

impl SubscriptionReader {
    async fn run(
        mut self,
        replay: Vec<Event>,
        mut events: futures::stream::BoxStream<'static, Result<Event, aion::EventStreamLagged>>,
    ) {
        // Replay phase: awaiting sends, gap-free by construction.
        for event in replay {
            if matches!(
                self.process(&event, QueueMode::Awaiting).await,
                ReaderStep::Stop
            ) {
                return;
            }
        }

        // Live phase: try_send lag semantics.
        while let Some(item) = events.next().await {
            // An engine-side lag item routes into the existing terminal
            // lagged path: one error frame, then close.
            let Ok(event) = item else {
                self.send_terminal(ServerError::lagged_stream().to_wire_error());
                return;
            };
            if matches!(
                self.process(&event, QueueMode::Bounded).await,
                ReaderStep::Stop
            ) {
                return;
            }
        }
    }

    async fn process(&mut self, event: &Event, mode: QueueMode) -> ReaderStep {
        let is_target = self
            .workflow_target
            .as_ref()
            .is_some_and(|target| event.workflow_id() == target);
        // FINDING L2 tripwire: never deliver a gapped per-workflow stream —
        // a contiguity violation is a loud typed terminal error instead.
        if is_target {
            if let Err(error) = self.contiguity.check(event) {
                self.send_terminal(error);
                return ReaderStep::Stop;
            }
        }
        match self.queue(event, mode).await {
            Ok(FrameOutcome::Delivered) => {
                if is_target {
                    self.contiguity.record_delivered(event);
                    if is_terminal_workflow_event(event) {
                        return ReaderStep::Stop;
                    }
                }
                ReaderStep::Continue
            }
            Ok(FrameOutcome::Filtered) => ReaderStep::Continue,
            Ok(FrameOutcome::Stop) | Err(()) => ReaderStep::Stop,
        }
    }

    /// Gate, select, encode, and queue one event. `Err(())` means a terminal
    /// error frame was already reported through the oneshot.
    async fn queue(&mut self, event: &Event, mode: QueueMode) -> Result<FrameOutcome, ()> {
        let workflow_type = match self.gate.admit(event).await {
            Ok(GateVerdict::Permitted { workflow_type }) => workflow_type,
            // Foreign/unknown workflow on the engine-global broadcast: never
            // this tenant's frame. Filtered out before encoding.
            Ok(GateVerdict::Filtered) => return Ok(FrameOutcome::Filtered),
            Err(error) => {
                self.send_terminal(error.to_wire_error());
                return Err(());
            }
        };
        // Server-side selector enforcement: workflow-type and status
        // selectors run on the same cached read that proved ownership.
        if !self.selector.matches(event, workflow_type.as_deref()) {
            return Ok(FrameOutcome::Filtered);
        }
        let frame = match encode_frame(&self.namespace, event) {
            Ok(frame) => frame,
            Err(error) => {
                self.send_terminal(error);
                return Err(());
            }
        };
        match mode {
            QueueMode::Awaiting => {
                if self.frames_tx.send(frame).await.is_err() {
                    return Ok(FrameOutcome::Stop);
                }
                Ok(FrameOutcome::Delivered)
            }
            QueueMode::Bounded => match self.frames_tx.try_send(frame) {
                Ok(()) => Ok(FrameOutcome::Delivered),
                Err(mpsc::error::TrySendError::Full(frame)) => {
                    drop(frame);
                    self.send_terminal(ServerError::lagged_stream().to_wire_error());
                    Err(())
                }
                Err(mpsc::error::TrySendError::Closed(frame)) => {
                    drop(frame);
                    Ok(FrameOutcome::Stop)
                }
            },
        }
    }

    fn send_terminal(&mut self, error: WireError) {
        if let Some(sender) = self.error_tx.take() {
            let send_result = sender.send(error);
            drop(send_result);
        }
    }
}

/// Per-workflow delivered-sequence tripwire.
///
/// Per-workflow subscriptions contract contiguous `seq` delivery; under the
/// splice invariants (single writer, publish-after-commit,
/// subscribe-then-snapshot) a gap is unreachable, so observing one means an
/// invariant was violated upstream. The guard converts that into a loud typed
/// terminal frame instead of a silently gapped stream; the client's standard
/// lagged recovery (reconnect with `resume_from_seq = last delivered + 1`)
/// re-reads durable history and is correct for gaps and regressions alike.
struct ContiguityGuard {
    last_delivered: Option<u64>,
}

impl ContiguityGuard {
    const fn new() -> Self {
        Self {
            last_delivered: None,
        }
    }

    /// Validate the next about-to-be-delivered target event. On `Err` the
    /// event must not be delivered.
    fn check(&self, event: &Event) -> Result<(), WireError> {
        let Some(last) = self.last_delivered else {
            // The first delivered event establishes the baseline (resume
            // cursors and mid-history live attaches both start anywhere).
            return Ok(());
        };
        let expected = last.saturating_add(1);
        let observed = event.seq();
        if observed == expected {
            return Ok(());
        }
        Err(WireError::new_with_type(
            WireErrorCode::Lagged,
            SEQUENCE_CONTIGUITY_VIOLATION,
            format!(
                "per-workflow stream contiguity violated: expected seq {expected}, observed seq \
                 {observed}; reconnect with resume_from_seq = {expected} to resume gap-free from \
                 recorded history"
            ),
        ))
    }

    fn record_delivered(&mut self, event: &Event) {
        self.last_delivered = Some(event.seq());
    }
}

fn encode_frame(namespace: &str, event: &Event) -> Result<EncodedFrame, WireError> {
    let frame = encode_streamed_event(namespace.to_owned(), None, event)?;
    serde_json::to_string(&frame).map_err(|source| {
        WireError::backend(format!(
            "failed to serialize streamed event frame: {source}"
        ))
    })
}

fn is_terminal_workflow_event(event: &Event) -> bool {
    matches!(
        event,
        Event::WorkflowCompleted { .. }
            | Event::WorkflowFailed { .. }
            | Event::WorkflowCancelled { .. }
            | Event::WorkflowTimedOut { .. }
            | Event::WorkflowContinuedAsNew { .. }
    )
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use aion::EventFilter;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId, WorkflowStatus};
    use aion_proto::{WireError, WireErrorCode};
    use axum::extract::ws::Message;
    use futures::{StreamExt, stream, stream::BoxStream};
    use serde_json::json;

    use super::{SEQUENCE_CONTIGUITY_VIOLATION, drive_socket, spawn_encoded_event_stream};
    use crate::config::NamespaceMode;
    use crate::error::ServerError;
    use crate::namespace::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};
    use crate::stream::namespace_filter::NamespaceEventGate;
    use crate::stream::selector::SubscriptionSelector;
    use crate::stream::subscribe::EventSubscription;

    fn capacity(value: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
        NonZeroUsize::new(value).ok_or_else(|| "capacity must be non-zero".into())
    }

    fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn started_with_type(
        seq: u64,
        workflow_id: &WorkflowId,
        workflow_type: &str,
    ) -> Result<aion_core::Event, aion_core::PayloadError> {
        Ok(aion_core::Event::WorkflowStarted {
            envelope: envelope(seq, workflow_id),
            workflow_type: workflow_type.to_owned(),
            input: payload("input")?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
        })
    }

    fn started(
        seq: u64,
        workflow_id: &WorkflowId,
    ) -> Result<aion_core::Event, aion_core::PayloadError> {
        started_with_type(seq, workflow_id, "checkout")
    }

    fn signal(
        seq: u64,
        workflow_id: &WorkflowId,
    ) -> Result<aion_core::Event, aion_core::PayloadError> {
        Ok(aion_core::Event::SignalReceived {
            envelope: envelope(seq, workflow_id),
            name: format!("signal-{seq}"),
            payload: payload("signal")?,
        })
    }

    fn completed(
        seq: u64,
        workflow_id: &WorkflowId,
    ) -> Result<aion_core::Event, aion_core::PayloadError> {
        Ok(aion_core::Event::WorkflowCompleted {
            envelope: envelope(seq, workflow_id),
            result: payload("result")?,
        })
    }

    fn tenant_a_gate(
        ownership: StaticWorkflowNamespaces,
    ) -> Result<NamespaceEventGate, Box<dyn std::error::Error>> {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
        Ok(NamespaceEventGate::new(
            resolver,
            "tenant-a".to_owned(),
            capacity(16)?,
        ))
    }

    fn subscription(
        workflow_target: Option<WorkflowId>,
        replay: Vec<Event>,
        events: BoxStream<'static, Result<Event, aion::EventStreamLagged>>,
    ) -> EventSubscription {
        selected_subscription(
            workflow_target,
            replay,
            events,
            SubscriptionSelector::unrestricted(),
        )
    }

    fn selected_subscription(
        workflow_target: Option<WorkflowId>,
        replay: Vec<Event>,
        events: BoxStream<'static, Result<Event, aion::EventStreamLagged>>,
        selector: SubscriptionSelector,
    ) -> EventSubscription {
        EventSubscription {
            namespace: "tenant-a".to_owned(),
            filter: EventFilter::default(),
            selector,
            workflow_target,
            replay,
            events,
        }
    }

    fn owned_gate(
        workflow_ids: &[&WorkflowId],
    ) -> Result<NamespaceEventGate, Box<dyn std::error::Error>> {
        let ownership = StaticWorkflowNamespaces::default();
        for workflow_id in workflow_ids {
            ownership.record((*workflow_id).clone(), "tenant-a")?;
        }
        tenant_a_gate(ownership)
    }

    async fn next_frame(
        receiver: &mut tokio::sync::mpsc::Receiver<String>,
    ) -> Result<Option<String>, tokio::time::error::Elapsed> {
        tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await
    }

    #[tokio::test]
    async fn per_workflow_stream_ends_after_terminal_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let events = stream::iter([
            Ok(started(1, &workflow_id)?),
            Ok(completed(2, &workflow_id)?),
            Ok(started(3, &workflow_id)?),
        ])
        .boxed();
        let mut stream = spawn_encoded_event_stream(
            subscription(Some(workflow_id.clone()), Vec::new(), events),
            owned_gate(&[&workflow_id])?,
            4,
        )?;

        let first = next_frame(&mut stream.frames).await?;
        let second = next_frame(&mut stream.frames).await?;
        let third = next_frame(&mut stream.frames).await?;

        assert!(first.is_some());
        assert!(second.is_some());
        assert!(third.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn dropping_receiver_cleans_up_subscription_reader()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let events =
            stream::iter([Ok(started(1, &workflow_id)?), Ok(signal(2, &workflow_id)?)]).boxed();
        let stream = spawn_encoded_event_stream(
            subscription(None, Vec::new(), events),
            owned_gate(&[&workflow_id])?,
            1,
        )?;
        drop(stream.frames);

        tokio::time::timeout(Duration::from_secs(1), stream.reader_done).await??;
        Ok(())
    }

    #[tokio::test]
    async fn slow_consumer_lags_without_blocking_fast_consumer()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let events: Vec<Result<aion_core::Event, aion::EventStreamLagged>> = vec![
            Ok(started(1, &workflow_id)?),
            Ok(signal(2, &workflow_id)?),
            Ok(completed(3, &workflow_id)?),
        ];
        let slow = spawn_encoded_event_stream(
            subscription(None, Vec::new(), stream::iter(events.clone()).boxed()),
            owned_gate(&[&workflow_id])?,
            1,
        )?;
        let mut fast = spawn_encoded_event_stream(
            subscription(None, Vec::new(), stream::iter(events).boxed()),
            owned_gate(&[&workflow_id])?,
            4,
        )?;

        let lag = tokio::time::timeout(Duration::from_secs(1), slow.lagged).await??;
        assert_eq!(lag.code, WireErrorCode::Lagged);

        let mut received = 0_usize;
        while let Some(frame) = next_frame(&mut fast.frames).await? {
            drop(frame);
            received += 1;
        }
        assert_eq!(received, 3);
        Ok(())
    }

    /// REVIEW RIDER 1: the broadcast channel is engine-global; a firehose
    /// subscription authorized for tenant-a must never observe tenant-b's
    /// events, and every delivered frame is labeled with the authorized
    /// namespace only because the gate proved ownership first.
    #[tokio::test]
    async fn firehose_never_delivers_foreign_namespace_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let foreign = WorkflowId::new(uuid::Uuid::from_u128(2));
        let unknown = WorkflowId::new(uuid::Uuid::from_u128(3));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(own.clone(), "tenant-a")?;
        ownership.record(foreign.clone(), "tenant-b")?;
        // The engine-global broadcast interleaves both tenants plus an
        // ownerless workflow.
        let events = stream::iter([
            Ok(started(1, &foreign)?),
            Ok(started(1, &own)?),
            Ok(started(1, &unknown)?),
            Ok(signal(2, &foreign)?),
            Ok(signal(2, &own)?),
        ])
        .boxed();
        let mut stream = spawn_encoded_event_stream(
            subscription(None, Vec::new(), events),
            tenant_a_gate(ownership)?,
            8,
        )?;

        let mut delivered = Vec::new();
        while let Some(frame) = next_frame(&mut stream.frames).await? {
            let streamed: aion_proto::StreamedEvent = serde_json::from_str(&frame)?;
            assert_eq!(streamed.namespace, "tenant-a");
            delivered.push(streamed.decode_event()?.workflow_id().clone());
        }
        assert_eq!(
            delivered,
            vec![own.clone(), own],
            "only tenant-a workflow events may be delivered"
        );
        Ok(())
    }

    /// FINDING M2: a `workflow_type` selector must deliver only matching
    /// workflows' events — including events first-sighted mid-stream whose
    /// type comes from the gate's cached durable read, not the event itself.
    #[tokio::test]
    async fn type_selector_delivers_only_matching_workflows_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let checkout = WorkflowId::new(uuid::Uuid::from_u128(1));
        let fulfillment = WorkflowId::new(uuid::Uuid::from_u128(2));
        let untyped = WorkflowId::new(uuid::Uuid::from_u128(3));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record_with_type(checkout.clone(), "tenant-a", "checkout")?;
        ownership.record_with_type(fulfillment.clone(), "tenant-a", "fulfillment")?;
        ownership.record(untyped.clone(), "tenant-a")?;
        let events = stream::iter([
            // First-sighted via a signal: type must come from the cached read.
            Ok(signal(5, &checkout)?),
            Ok(signal(5, &fulfillment)?),
            Ok(signal(5, &untyped)?),
            Ok(started_with_type(6, &checkout, "checkout")?),
            Ok(started_with_type(6, &fulfillment, "fulfillment")?),
        ])
        .boxed();
        let mut stream = spawn_encoded_event_stream(
            selected_subscription(
                None,
                Vec::new(),
                events,
                SubscriptionSelector {
                    workflow_type: Some("checkout".to_owned()),
                    status: None,
                },
            ),
            tenant_a_gate(ownership)?,
            8,
        )?;

        let mut delivered = Vec::new();
        while let Some(frame) = next_frame(&mut stream.frames).await? {
            let streamed: aion_proto::StreamedEvent = serde_json::from_str(&frame)?;
            delivered.push(streamed.decode_event()?.workflow_id().clone());
        }
        assert_eq!(
            delivered,
            vec![checkout.clone(), checkout],
            "only events of workflows with the selected type may be delivered"
        );
        Ok(())
    }

    /// FINDING M2: a `status` selector delivers per the documented event-kind
    /// rule — terminal events match their projected status, `Running` matches
    /// non-terminal events.
    #[tokio::test]
    async fn status_selector_delivers_per_event_kind_rule() -> Result<(), Box<dyn std::error::Error>>
    {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let make_events = || -> Result<_, aion_core::PayloadError> {
            Ok(stream::iter([
                Ok(started(1, &workflow_id)?),
                Ok(signal(2, &workflow_id)?),
                Ok(completed(3, &workflow_id)?),
            ])
            .boxed())
        };

        let mut completed_only = spawn_encoded_event_stream(
            selected_subscription(
                None,
                Vec::new(),
                make_events()?,
                SubscriptionSelector {
                    workflow_type: None,
                    status: Some(WorkflowStatus::Completed),
                },
            ),
            owned_gate(&[&workflow_id])?,
            8,
        )?;
        let mut delivered = Vec::new();
        while let Some(frame) = next_frame(&mut completed_only.frames).await? {
            let streamed: aion_proto::StreamedEvent = serde_json::from_str(&frame)?;
            delivered.push(streamed.decode_event()?.seq());
        }
        assert_eq!(
            delivered,
            vec![3],
            "status=Completed delivers only the WorkflowCompleted event"
        );

        let mut running_only = spawn_encoded_event_stream(
            selected_subscription(
                None,
                Vec::new(),
                make_events()?,
                SubscriptionSelector {
                    workflow_type: None,
                    status: Some(WorkflowStatus::Running),
                },
            ),
            owned_gate(&[&workflow_id])?,
            8,
        )?;
        let mut delivered = Vec::new();
        while let Some(frame) = next_frame(&mut running_only.frames).await? {
            let streamed: aion_proto::StreamedEvent = serde_json::from_str(&frame)?;
            delivered.push(streamed.decode_event()?.seq());
        }
        assert_eq!(
            delivered,
            vec![1, 2],
            "status=Running delivers exactly the non-terminal events"
        );
        Ok(())
    }

    /// FINDING M2: combined selectors AND together.
    #[tokio::test]
    async fn combined_selectors_and_together() -> Result<(), Box<dyn std::error::Error>> {
        let checkout = WorkflowId::new(uuid::Uuid::from_u128(1));
        let fulfillment = WorkflowId::new(uuid::Uuid::from_u128(2));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record_with_type(checkout.clone(), "tenant-a", "checkout")?;
        ownership.record_with_type(fulfillment.clone(), "tenant-a", "fulfillment")?;
        let events = stream::iter([
            Ok(signal(1, &checkout)?),
            Ok(completed(2, &fulfillment)?),
            Ok(completed(2, &checkout)?),
        ])
        .boxed();
        let mut stream = spawn_encoded_event_stream(
            selected_subscription(
                None,
                Vec::new(),
                events,
                SubscriptionSelector {
                    workflow_type: Some("checkout".to_owned()),
                    status: Some(WorkflowStatus::Completed),
                },
            ),
            tenant_a_gate(ownership)?,
            8,
        )?;

        let mut delivered = Vec::new();
        while let Some(frame) = next_frame(&mut stream.frames).await? {
            let streamed: aion_proto::StreamedEvent = serde_json::from_str(&frame)?;
            let event = streamed.decode_event()?;
            delivered.push((event.workflow_id().clone(), event.seq()));
        }
        assert_eq!(
            delivered,
            vec![(checkout, 2)],
            "only the selected type's terminal event may pass both selectors"
        );
        Ok(())
    }

    /// A replay longer than the outbound buffer must be delivered completely
    /// via awaiting sends — never dropped, never a spurious lag.
    #[tokio::test]
    async fn replay_longer_than_outbound_buffer_is_delivered_without_lag()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let mut replay: Vec<Event> = vec![started(1, &workflow_id)?];
        for seq in 2..=6 {
            replay.push(signal(seq, &workflow_id)?);
        }
        let mut stream = spawn_encoded_event_stream(
            subscription(Some(workflow_id.clone()), replay, stream::empty().boxed()),
            owned_gate(&[&workflow_id])?,
            2,
        )?;

        let mut received = 0_usize;
        while let Some(frame) = next_frame(&mut stream.frames).await? {
            drop(frame);
            received += 1;
        }
        assert_eq!(received, 6, "all replay frames must arrive despite bound 2");
        let lag = tokio::time::timeout(Duration::from_secs(1), stream.lagged).await?;
        assert!(lag.is_err(), "replay must not produce a lag error");
        Ok(())
    }

    /// FINDING L2: a gapped live tail on a per-workflow subscription is a loud
    /// typed terminal error — the gapped event is never delivered silently.
    #[tokio::test]
    async fn gapped_per_workflow_stream_is_terminal_error_never_silent_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        // Deliberately gapped: 1, 2, then 4.
        let events = stream::iter([
            Ok(started(1, &workflow_id)?),
            Ok(signal(2, &workflow_id)?),
            Ok(signal(4, &workflow_id)?),
        ])
        .boxed();
        let mut stream = spawn_encoded_event_stream(
            subscription(Some(workflow_id.clone()), Vec::new(), events),
            owned_gate(&[&workflow_id])?,
            8,
        )?;

        let mut delivered = Vec::new();
        while let Some(frame) = next_frame(&mut stream.frames).await? {
            let streamed: aion_proto::StreamedEvent = serde_json::from_str(&frame)?;
            delivered.push(streamed.decode_event()?.seq());
        }
        assert_eq!(delivered, vec![1, 2], "the gapped event must never deliver");

        let error = tokio::time::timeout(Duration::from_secs(1), stream.lagged).await??;
        assert_eq!(error.code, WireErrorCode::Lagged);
        assert_eq!(
            error.error_type.as_deref(),
            Some(SEQUENCE_CONTIGUITY_VIOLATION)
        );
        Ok(())
    }

    /// FINDING L2: the tripwire spans the replay→live boundary and also trips
    /// on regressions (duplicate seq), not just gaps.
    #[tokio::test]
    async fn contiguity_tripwire_spans_replay_live_boundary_and_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();

        // Replay ends at 2; live starts at 4: gap across the boundary.
        let gapped = spawn_encoded_event_stream(
            subscription(
                Some(workflow_id.clone()),
                vec![started(1, &workflow_id)?, signal(2, &workflow_id)?],
                stream::iter([Ok(signal(4, &workflow_id)?)]).boxed(),
            ),
            owned_gate(&[&workflow_id])?,
            8,
        )?;
        let error = tokio::time::timeout(Duration::from_secs(1), gapped.lagged).await??;
        assert_eq!(
            error.error_type.as_deref(),
            Some(SEQUENCE_CONTIGUITY_VIOLATION)
        );

        // Live re-emits the already-delivered seq 2: regression trips too.
        let duplicated = spawn_encoded_event_stream(
            subscription(
                Some(workflow_id.clone()),
                vec![started(1, &workflow_id)?, signal(2, &workflow_id)?],
                stream::iter([Ok(signal(2, &workflow_id)?)]).boxed(),
            ),
            owned_gate(&[&workflow_id])?,
            8,
        )?;
        let error = tokio::time::timeout(Duration::from_secs(1), duplicated.lagged).await??;
        assert_eq!(
            error.error_type.as_deref(),
            Some(SEQUENCE_CONTIGUITY_VIOLATION)
        );
        Ok(())
    }

    /// Collected sink messages from one `drive_socket` run.
    async fn run_drive_socket(
        frames: tokio::sync::mpsc::Receiver<String>,
        lagged: tokio::sync::oneshot::Receiver<WireError>,
    ) -> Result<(Vec<Message>, Result<(), ServerError>), Box<dyn std::error::Error>> {
        let mut frames = frames;
        let (mut sink, collected) = futures::channel::mpsc::unbounded();
        let mut socket_rx = stream::pending::<Result<Message, axum::Error>>();
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            drive_socket(&mut sink, &mut socket_rx, &mut frames, lagged),
        )
        .await?;
        drop(sink);
        let messages: Vec<Message> = collected.collect().await;
        Ok((messages, outcome))
    }

    fn assert_frames_then_error_then_close(
        messages: &[Message],
        expected_frames: usize,
        expected_code: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            messages.len(),
            expected_frames + 2,
            "expected {expected_frames} event frames + error frame + close, got {messages:?}"
        );
        for message in &messages[..expected_frames] {
            let Message::Text(text) = message else {
                return Err(format!("expected an event text frame, got {message:?}").into());
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str())?;
            assert!(
                value.get("error").is_none(),
                "event frames must precede the error frame"
            );
        }
        let Message::Text(text) = &messages[expected_frames] else {
            return Err("expected the terminal error text frame".into());
        };
        let value: serde_json::Value = serde_json::from_str(text.as_str())?;
        assert_eq!(value["error"]["code"], json!(expected_code));
        let Message::Close(Some(close)) = &messages[expected_frames + 1] else {
            return Err("expected a close frame after the error frame".into());
        };
        assert_eq!(close.reason.as_str(), expected_code);
        Ok(())
    }

    /// FINDING M1: when the reader queues frames, fires the terminal oneshot,
    /// and drops the frame sender, both `select!` branches are ready and the
    /// winner is random — yet the client must always receive every buffered
    /// frame, then exactly one terminal error frame, then close. Constructed
    /// with both branches ready before the loop starts and repeated to cover
    /// both orderings.
    #[tokio::test]
    async fn terminal_error_and_buffered_frames_are_never_lost_regardless_of_select_order()
    -> Result<(), Box<dyn std::error::Error>> {
        for _ in 0..64 {
            let (frames_tx, frames_rx) = tokio::sync::mpsc::channel::<String>(8);
            let (lag_tx, lag_rx) = tokio::sync::oneshot::channel::<WireError>();
            // Reader ordering contract: frames queued, then oneshot fired,
            // then sender dropped — reproduced here with both select branches
            // ready before drive_socket polls either.
            for seq in 1..=3 {
                frames_tx
                    .send(json!({ "seq": seq }).to_string())
                    .await
                    .map_err(|_| "frame channel must accept the fixture frames")?;
            }
            lag_tx
                .send(WireError::lagged("subscriber lagged behind"))
                .map_err(|_| "oneshot must accept the terminal error")?;
            drop(frames_tx);

            let (messages, outcome) = run_drive_socket(frames_rx, lag_rx).await?;
            assert_frames_then_error_then_close(&messages, 3, "lagged")?;
            let error = outcome.err().ok_or("terminal stream must surface Err")?;
            assert_eq!(error.to_wire_error().code, WireErrorCode::Lagged);
        }
        Ok(())
    }

    /// FINDING M1 end-to-end through the real reader: N events then an
    /// engine-side lag item must always deliver N frames, then the lagged
    /// error frame, then close — across repeated runs with arbitrary task
    /// interleaving.
    #[tokio::test]
    async fn reader_lag_after_events_delivers_all_frames_then_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        for _ in 0..32 {
            let events = stream::iter([
                Ok(started(1, &workflow_id)?),
                Ok(signal(2, &workflow_id)?),
                Ok(signal(3, &workflow_id)?),
                Err(aion::EventStreamLagged { skipped: 7 }),
            ])
            .boxed();
            let encoded = spawn_encoded_event_stream(
                subscription(Some(workflow_id.clone()), Vec::new(), events),
                owned_gate(&[&workflow_id])?,
                8,
            )?;
            let (messages, outcome) = run_drive_socket(encoded.frames, encoded.lagged).await?;
            assert_frames_then_error_then_close(&messages, 3, "lagged")?;
            assert!(outcome.is_err(), "lagged stream must surface Err");
            encoded.reader_done.abort();
        }
        Ok(())
    }

    /// A clean reader end (no terminal error) delivers the buffered frames and
    /// returns Ok without inventing an error frame.
    #[tokio::test]
    async fn clean_stream_end_delivers_frames_without_error_frame()
    -> Result<(), Box<dyn std::error::Error>> {
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel::<String>(8);
        let (lag_tx, lag_rx) = tokio::sync::oneshot::channel::<WireError>();
        for seq in 1..=2 {
            frames_tx
                .send(json!({ "seq": seq }).to_string())
                .await
                .map_err(|_| "frame channel must accept the fixture frames")?;
        }
        drop(frames_tx);
        drop(lag_tx);

        let (messages, outcome) = run_drive_socket(frames_rx, lag_rx).await?;
        assert!(outcome.is_ok(), "clean end must not surface an error");
        assert_eq!(
            messages.len(),
            2,
            "exactly the event frames, no error/close"
        );
        for message in &messages {
            let Message::Text(text) = message else {
                return Err(format!("expected a text frame, got {message:?}").into());
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str())?;
            assert!(value.get("error").is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn wire_error_frame_is_wrapped_and_followed_by_close()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut sink, collected) = futures::channel::mpsc::unbounded();
        let error = crate::error::ServerError::lagged_stream().to_wire_error();

        super::send_wire_error(&mut sink, &error).await?;
        drop(sink);

        let messages: Vec<axum::extract::ws::Message> = collected.collect().await;
        assert_eq!(
            messages.len(),
            2,
            "expected exactly one error frame + close"
        );

        let axum::extract::ws::Message::Text(text) = &messages[0] else {
            return Err("expected a text error frame".into());
        };
        let frame: serde_json::Value = serde_json::from_str(text.as_str())?;
        assert_eq!(frame["error"]["code"], json!("lagged"));
        assert!(
            frame["error"]["message"].is_string(),
            "error frame must carry the informational message"
        );

        let axum::extract::ws::Message::Close(Some(close)) = &messages[1] else {
            return Err("expected a close frame after the error frame".into());
        };
        assert_eq!(close.reason.as_str(), "lagged");
        Ok(())
    }
}
