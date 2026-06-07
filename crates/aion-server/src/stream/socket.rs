//! WebSocket forward loop + lag handling.

use aion_core::{Event, WorkflowId};
use aion_proto::SubscriptionRequest;
use aion_proto::{WireError, encode_streamed_event};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{SinkExt, StreamExt, stream::BoxStream};
use tokio::sync::{mpsc, oneshot};

use crate::error::ServerError;
use crate::namespace::CallerIdentity;
use crate::state::ServerState;
use crate::stream::subscribe::subscribe_events;

/// Encoded event frame queued for a WebSocket connection.
pub type EncodedFrame = String;

/// Authorize a wire subscription request and forward it on an accepted socket.
///
/// The per-connection buffer bound is read from runtime config, not defaulted in
/// the transport loop.
///
/// # Errors
///
/// Returns [`ServerError`] when namespace authorization, engine subscription,
/// frame serialization, or bounded-buffer forwarding fails.
pub async fn handle_subscription_socket(
    socket: WebSocket,
    state: &ServerState,
    caller: &CallerIdentity,
    request: &SubscriptionRequest,
) -> Result<(), ServerError> {
    let subscription = subscribe_events(state.namespace_guard(), caller, request)?;
    let outbound_buffer_bound = state.runtime_config().websocket.outbound_buffer_bound;
    forward_subscription(
        socket,
        subscription.namespace,
        subscription.workflow_target,
        subscription.events,
        outbound_buffer_bound,
    )
    .await
}

/// Forward a previously authorized engine subscription to a WebSocket.
///
/// # Errors
///
/// Returns [`ServerError::Stream`] when the bounded outbound buffer reports lag,
/// or [`ServerError::Wire`] when a streamed event/error frame cannot be encoded.
pub async fn forward_subscription(
    socket: WebSocket,
    namespace: String,
    workflow_target: Option<WorkflowId>,
    events: BoxStream<'static, Event>,
    outbound_buffer_bound: usize,
) -> Result<(), ServerError> {
    let EncodedEventStream {
        mut frames,
        lagged,
        reader_done,
    } = spawn_encoded_event_stream(namespace, workflow_target, events, outbound_buffer_bound)?;
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

async fn send_wire_error<S>(socket_tx: &mut S, error: &WireError) -> Result<(), ServerError>
where
    S: futures::Sink<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    let payload = serde_json::to_string(error).map_err(|source| ServerError::Wire {
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
    /// Receives a typed lag error if the bounded frame queue fills.
    pub lagged: oneshot::Receiver<WireError>,
    /// Reader task owning the upstream engine stream.
    pub reader_done: tokio::task::JoinHandle<()>,
}

/// Spawn the non-blocking engine-reader side of a WebSocket subscription.
///
/// The reader uses `try_send` into the bounded per-connection channel; it never
/// awaits capacity from socket I/O and therefore never back-pressures the engine
/// event tail for a slow consumer.
///
/// # Errors
///
/// Returns `ServerError::Config` if `outbound_buffer_bound` is zero.
pub fn spawn_encoded_event_stream(
    namespace: String,
    workflow_target: Option<WorkflowId>,
    mut events: BoxStream<'static, Event>,
    outbound_buffer_bound: usize,
) -> Result<EncodedEventStream, ServerError> {
    if outbound_buffer_bound == 0 {
        return Err(ServerError::Config {
            message: "websocket.outbound_buffer_bound must be greater than zero".to_owned(),
        });
    }

    let (frames_tx, frames) = mpsc::channel(outbound_buffer_bound);
    let (lag_tx, lagged) = oneshot::channel();
    let reader_done = tokio::spawn(async move {
        let mut lag_tx = Some(lag_tx);
        while let Some(event) = events.next().await {
            let terminal = workflow_target.as_ref().is_some_and(|target| {
                event.workflow_id() == target && is_terminal_workflow_event(&event)
            });
            let frame = match encode_frame(&namespace, &event) {
                Ok(frame) => frame,
                Err(error) => {
                    if let Some(sender) = lag_tx.take() {
                        let send_result = sender.send(error);
                        drop(send_result);
                    }
                    break;
                }
            };
            match frames_tx.try_send(frame) {
                Ok(()) if terminal => break,
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(frame)) => {
                    drop(frame);
                    if let Some(sender) = lag_tx.take() {
                        let send_result = sender.send(ServerError::lagged_stream().to_wire_error());
                        drop(send_result);
                    }
                    break;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(frame)) => {
                    drop(frame);
                    break;
                }
            }
        }
    });

    Ok(EncodedEventStream {
        frames,
        lagged,
        reader_done,
    })
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

    use aion_core::{EventEnvelope, Payload, WorkflowId};
    use futures::{StreamExt, stream};
    use serde_json::json;

    use super::spawn_encoded_event_stream;

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
            started(1, &workflow_id)?,
            completed(2, &workflow_id)?,
            started(3, &workflow_id)?,
        ])
        .boxed();
        let mut stream =
            spawn_encoded_event_stream("tenant-a".to_owned(), Some(workflow_id), events, 4)?;

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
        let events = stream::iter([started(1, &workflow_id)?, started(2, &workflow_id)?]).boxed();
        let stream = spawn_encoded_event_stream("tenant-a".to_owned(), None, events, 1)?;
        drop(stream.frames);

        tokio::time::timeout(Duration::from_secs(1), stream.reader_done).await??;
        Ok(())
    }

    #[tokio::test]
    async fn slow_consumer_lags_without_blocking_fast_consumer()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let events = vec![
            started(1, &workflow_id)?,
            started(2, &workflow_id)?,
            completed(3, &workflow_id)?,
        ];
        let slow = spawn_encoded_event_stream(
            "tenant-a".to_owned(),
            None,
            stream::iter(events.clone()).boxed(),
            1,
        )?;
        let mut fast = spawn_encoded_event_stream(
            "tenant-a".to_owned(),
            None,
            stream::iter(events).boxed(),
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

    use aion_proto::WireErrorCode;
}
