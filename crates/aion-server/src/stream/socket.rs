//! WebSocket forward loop + lag handling.

use aion_core::Event;
use aion_proto::SubscriptionRequest;
use aion_proto::{WireError, encode_streamed_event};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};

use crate::error::ServerError;
use crate::namespace::CallerIdentity;
use crate::state::ServerState;
use crate::stream::namespace_filter::NamespaceEventGate;
use crate::stream::subscribe::{EventSubscription, subscribe_events};

/// Encoded event frame queued for a WebSocket connection.
pub type EncodedFrame = String;

/// Authorize a wire subscription request and forward it on an accepted socket.
///
/// The per-connection buffer bound is read from runtime config, not defaulted in
/// the transport loop.
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
    // The broadcast channel is engine-global with no namespace dimension, so
    // every delivered event passes the namespace gate before encoding. The
    // guard-verified per-workflow target is pre-seeded as allowed.
    let mut gate = NamespaceEventGate::new(
        state.namespace_guard().resolver().clone(),
        subscription.namespace.clone(),
    );
    if let Some(target) = &subscription.workflow_target {
        gate.allow(target.clone());
    }
    let outbound_buffer_bound = state.runtime_config().websocket.outbound_buffer_bound;
    forward_subscription(socket, subscription, gate, outbound_buffer_bound).await
}

/// Forward a previously authorized engine subscription to a WebSocket.
///
/// # Errors
///
/// Returns [`ServerError::Stream`] when the bounded outbound buffer reports lag,
/// or [`ServerError::Wire`] when a streamed event/error frame cannot be encoded.
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
    tokio::pin!(lagged);
    let mut lag_error = None;
    let mut lag_closed = false;

    loop {
        tokio::select! {
            client_message = socket_rx.next() => {
                match client_message {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(message)) => drop(message),
                    Some(Err(error)) => {
                        drop(error);
                        break;
                    }
                }
            }
            lag = &mut lagged, if !lag_closed && lag_error.is_none() => {
                match lag {
                    Ok(error) => {
                        send_wire_error(&mut socket_tx, &error).await?;
                        lag_error = Some(ServerError::lagged_stream());
                        break;
                    }
                    Err(_closed) => {
                        lag_closed = true;
                    }
                }
            }
            frame = frames.recv() => {
                let Some(frame) = frame else {
                    break;
                };
                if socket_tx.send(Message::Text(frame.into())).await.is_err() {
                    break;
                }
            }
        }
    }

    reader_done.abort();
    if let Some(error) = lag_error {
        Err(error)
    } else {
        Ok(())
    }
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
    /// engine-side lag item arrives, encoding fails, or the namespace gate's
    /// ownership source fails.
    pub lagged: oneshot::Receiver<WireError>,
    /// Reader task owning the upstream engine stream.
    pub reader_done: tokio::task::JoinHandle<()>,
}

/// Outcome of queuing one frame from the reader task.
enum FrameOutcome {
    /// Frame queued for delivery.
    Delivered,
    /// Event filtered out by the namespace gate; nothing queued.
    Filtered,
    /// Receiver gone: stop reading.
    Stop,
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
/// namespace gate before its frame is encoded.
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
        mut events,
        filter: _,
    } = subscription;
    let mut gate = gate;
    let (frames_tx, frames) = mpsc::channel(outbound_buffer_bound);
    let (lag_tx, lagged) = oneshot::channel();
    let reader_done = tokio::spawn(async move {
        let mut error_tx = Some(lag_tx);

        // Replay phase: awaiting sends, gap-free by construction.
        for event in replay {
            match queue_event(
                &namespace,
                &mut gate,
                &mut error_tx,
                &frames_tx,
                &event,
                QueueMode::Awaiting,
            )
            .await
            {
                Ok(FrameOutcome::Delivered) => {
                    if is_terminal_for_target(workflow_target.as_ref(), &event) {
                        return;
                    }
                }
                Ok(FrameOutcome::Filtered) => {}
                Ok(FrameOutcome::Stop) | Err(()) => return,
            }
        }

        // Live phase: try_send lag semantics, unchanged.
        while let Some(item) = events.next().await {
            // An engine-side lag item routes into the existing terminal
            // lagged path: one error frame, then close.
            let Ok(event) = item else {
                send_terminal(&mut error_tx, ServerError::lagged_stream().to_wire_error());
                return;
            };
            match queue_event(
                &namespace,
                &mut gate,
                &mut error_tx,
                &frames_tx,
                &event,
                QueueMode::Bounded,
            )
            .await
            {
                Ok(FrameOutcome::Delivered) => {
                    if is_terminal_for_target(workflow_target.as_ref(), &event) {
                        return;
                    }
                }
                Ok(FrameOutcome::Filtered) => {}
                Ok(FrameOutcome::Stop) | Err(()) => return,
            }
        }
    });

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

/// Gate, encode, and queue one event. `Err(())` means a terminal error frame
/// was already reported through `error_tx`.
async fn queue_event(
    namespace: &str,
    gate: &mut NamespaceEventGate,
    error_tx: &mut Option<oneshot::Sender<WireError>>,
    frames_tx: &mpsc::Sender<EncodedFrame>,
    event: &Event,
    mode: QueueMode,
) -> Result<FrameOutcome, ()> {
    match gate.permits(event).await {
        Ok(true) => {}
        // Foreign/unknown workflow on the engine-global broadcast: never this
        // tenant's frame. Filtered out before encoding.
        Ok(false) => return Ok(FrameOutcome::Filtered),
        Err(error) => {
            send_terminal(error_tx, error.to_wire_error());
            return Err(());
        }
    }
    let frame = match encode_frame(namespace, event) {
        Ok(frame) => frame,
        Err(error) => {
            send_terminal(error_tx, error);
            return Err(());
        }
    };
    match mode {
        QueueMode::Awaiting => {
            if frames_tx.send(frame).await.is_err() {
                return Ok(FrameOutcome::Stop);
            }
            Ok(FrameOutcome::Delivered)
        }
        QueueMode::Bounded => match frames_tx.try_send(frame) {
            Ok(()) => Ok(FrameOutcome::Delivered),
            Err(mpsc::error::TrySendError::Full(frame)) => {
                drop(frame);
                send_terminal(error_tx, ServerError::lagged_stream().to_wire_error());
                Err(())
            }
            Err(mpsc::error::TrySendError::Closed(frame)) => {
                drop(frame);
                Ok(FrameOutcome::Stop)
            }
        },
    }
}

fn send_terminal(error_tx: &mut Option<oneshot::Sender<WireError>>, error: WireError) {
    if let Some(sender) = error_tx.take() {
        let send_result = sender.send(error);
        drop(send_result);
    }
}

fn is_terminal_for_target(target: Option<&aion_core::WorkflowId>, event: &Event) -> bool {
    target.is_some_and(|target| event.workflow_id() == target && is_terminal_workflow_event(event))
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
    use std::time::Duration;

    use aion::EventFilter;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use futures::{StreamExt, stream, stream::BoxStream};
    use serde_json::json;

    use super::spawn_encoded_event_stream;
    use crate::config::NamespaceMode;
    use crate::namespace::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};
    use crate::stream::namespace_filter::NamespaceEventGate;
    use crate::stream::subscribe::EventSubscription;

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

    fn started(
        seq: u64,
        workflow_id: &WorkflowId,
    ) -> Result<aion_core::Event, aion_core::PayloadError> {
        Ok(aion_core::Event::WorkflowStarted {
            envelope: envelope(seq, workflow_id),
            workflow_type: "checkout".to_owned(),
            input: payload("input")?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
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

    fn tenant_a_gate(ownership: StaticWorkflowNamespaces) -> NamespaceEventGate {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
        NamespaceEventGate::new(resolver, "tenant-a".to_owned())
    }

    fn subscription(
        workflow_target: Option<WorkflowId>,
        replay: Vec<Event>,
        events: BoxStream<'static, Result<Event, aion::EventStreamLagged>>,
    ) -> EventSubscription {
        EventSubscription {
            namespace: "tenant-a".to_owned(),
            filter: EventFilter::default(),
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
        Ok(tenant_a_gate(ownership))
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
            stream::iter([Ok(started(1, &workflow_id)?), Ok(started(2, &workflow_id)?)]).boxed();
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
            Ok(started(2, &workflow_id)?),
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
            Ok(started(2, &foreign)?),
            Ok(started(2, &own)?),
        ])
        .boxed();
        let mut stream = spawn_encoded_event_stream(
            subscription(None, Vec::new(), events),
            tenant_a_gate(ownership),
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

    /// A replay longer than the outbound buffer must be delivered completely
    /// via awaiting sends — never dropped, never a spurious lag.
    #[tokio::test]
    async fn replay_longer_than_outbound_buffer_is_delivered_without_lag()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let replay: Vec<Event> = (1..=6)
            .map(|seq| started(seq, &workflow_id))
            .collect::<Result<_, _>>()?;
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

    use aion_proto::WireErrorCode;
}
