//! NOI-5b transcript subscription: namespace-gated durable-tail + live-splice
//! forward loop for one `(workflow, activity, attempt)` agent transcript.
//!
//! This is the agent-observability counterpart to [`super::cluster_stream`]'s
//! forward loop, and a NEW ARM on the existing single subscription frame of
//! `/events/stream` (the socket stays one-subscription-per-socket; there is no
//! multiplexing layer). A client that wants both a workflow stream and a
//! transcript opens two `/events/stream` sockets.
//!
//! # Authorization: namespace-scoped (like the per-workflow event stream)
//!
//! A transcript belongs to the workflow the activity runs under, so it is
//! authorized exactly like the per-workflow event subscription: the caller must
//! hold a grant for the transcript's `namespace` AND the target `workflow_id`
//! must be visible in it. The gate reuses the SAME
//! [`NamespaceGuard::scope`](crate::namespace::NamespaceGuard::scope) +
//! [`SubscriptionScope::PerWorkflow`](crate::namespace::SubscriptionScope) path
//! the workflow stream uses, so a caller probing a foreign or nonexistent
//! workflow receives the guard's anti-leak `not_found`, never a transcript.
//!
//! # Splice contract (gap-free, no duplicate)
//!
//! Mirrors the workflow resume path: attach the live broadcast BEFORE reading the
//! durable `O` tail (subscribe-then-replay), so an event that races the priming
//! read is retained by the receiver and applied after it, deduped on `store_seq`.
//! Ephemeral token deltas (`store_seq: None`) are forwarded live and never
//! replayed.

use aion_core::ActivityId;
use aion_proto::{
    PerWorkflowSubscription, ProtoWorkflowId, StreamedActivityEvent, TranscriptSubscription,
    WireError,
};
use aion_store::ActivityStreamKey;
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{SinkExt, StreamExt};

use crate::activity_publisher::TranscriptStreamLagged;
use crate::error::ServerError;
use crate::namespace::{CallerIdentity, NamespaceOperation, SubscriptionScope, WorkflowTarget};
use crate::state::ServerState;

/// Serve a transcript subscription on an already-upgraded socket.
///
/// Flow: decode + namespace-gate FIRST (anti-leak: a denied caller receives one
/// terminal wire-error frame + close, byte-identical to the workflow path); then
/// attach the live broadcast BEFORE reading the durable `O` tail (gap-free
/// splice); replay the durable tail; forward live events until the client closes
/// or the subscriber lags (one typed `transcript_lagged` frame then close).
///
/// # Errors
///
/// Returns [`ServerError`] when decode/authorization fails (after the terminal
/// frame is sent), the durable replay read fails, or the stream ends with a lag
/// terminal frame.
pub async fn serve_transcript_socket(
    mut socket: WebSocket,
    state: &ServerState,
    caller: &CallerIdentity,
    subscription: &TranscriptSubscription,
) -> Result<(), ServerError> {
    let key = match authorize_transcript(state, caller, subscription).await {
        Ok(key) => key,
        Err(error) => {
            super::socket::send_wire_error(&mut socket, &error.to_wire_error()).await?;
            return Err(error);
        }
    };

    let publisher = state.transcript_publisher();
    // T0: attach the live tail BEFORE reading the durable replay, so an event
    // emitted between the replay read and the first live poll is retained by the
    // receiver and applied after the replay (deduped on `store_seq`).
    let mut live = publisher.subscribe(key.clone(), subscription.after_seq);

    // T1 (> T0): replay the durable `O` tail from the resume cursor. `after_seq`
    // is the highest already-applied `store_seq`; replay everything strictly
    // after it (`None` replays the full transcript from `store_seq == 0`).
    let from_seq = subscription
        .after_seq
        .map_or(0, |seq| seq.saturating_add(1));
    let replay = publisher
        .replay_from(&key, from_seq)
        .await
        .map_err(ServerError::from)?;
    for record in replay {
        if send_activity_frame(&mut socket, record.event)
            .await?
            .is_break()
        {
            return Ok(());
        }
    }

    let (mut socket_tx, mut socket_rx) = socket.split();
    loop {
        tokio::select! {
            client_message = socket_rx.next() => {
                match client_message {
                    // Close, socket error, or any inbound frame ends the read
                    // side: the transcript channel takes no further client
                    // frames (one-subscription-per-socket), so an inbound frame
                    // after subscribe is a benign close.
                    Some(Ok(Message::Close(_))) | None => {
                        return send_normal_close(&mut socket_tx).await;
                    }
                    Some(Ok(_other)) => {}
                    Some(Err(_error)) => return Ok(()),
                }
            }
            item = live.next() => {
                match item {
                    Some(Ok(event)) => {
                        if forward_live_frame(&mut socket_tx, event).await?.is_break() {
                            return Ok(());
                        }
                    }
                    Some(Err(TranscriptStreamLagged { skipped })) => {
                        return deliver_transcript_terminal(&mut socket_tx, skipped).await;
                    }
                    None => return send_normal_close(&mut socket_tx).await,
                }
            }
        }
    }
}

/// Decode the transcript identifiers and namespace-gate the caller, returning the
/// authorized `(workflow, activity, attempt)` stream key.
///
/// Reuses the per-workflow subscription scope so authorization is byte-identical
/// to the workflow event stream: the caller must hold the namespace grant AND the
/// workflow must be visible in it (anti-leak `not_found` otherwise).
async fn authorize_transcript(
    state: &ServerState,
    caller: &CallerIdentity,
    subscription: &TranscriptSubscription,
) -> Result<ActivityStreamKey, ServerError> {
    let workflow_id = decode_workflow_id(subscription.workflow_id.as_ref())?;
    let activity_id = decode_activity_id(subscription)?;
    let per_workflow = PerWorkflowSubscription {
        namespace: subscription.namespace.clone(),
        workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
        resume_from_seq: None,
    };
    let target = WorkflowTarget::workflow(&workflow_id);
    let scope = SubscriptionScope::PerWorkflow(&per_workflow, target);
    let filter = aion::EventFilter {
        workflow_id: Some(workflow_id.clone()),
        ..aion::EventFilter::default()
    };
    let operation = NamespaceOperation::subscribe(scope, &filter);
    // Guard verdict FIRST: nothing below runs for an unauthorized caller.
    state.namespace_guard().scope(caller, &operation).await?;
    Ok(ActivityStreamKey::new(
        workflow_id,
        activity_id,
        subscription.attempt,
    ))
}

fn decode_workflow_id(
    workflow_id: Option<&ProtoWorkflowId>,
) -> Result<aion_core::WorkflowId, ServerError> {
    workflow_id
        .cloned()
        .ok_or_else(|| ServerError::Wire {
            wire: WireError::invalid_input("transcript subscription workflow_id is missing"),
        })?
        .try_into()
        .map_err(|wire| ServerError::Wire { wire })
}

fn decode_activity_id(subscription: &TranscriptSubscription) -> Result<ActivityId, ServerError> {
    let activity_id = subscription.activity_id.ok_or_else(|| ServerError::Wire {
        wire: WireError::invalid_input("transcript subscription activity_id is missing"),
    })?;
    Ok(ActivityId::from(activity_id))
}

/// Serialize + send one transcript event on the still-unified socket (during the
/// durable replay, before the read/write split). A send failure means the client
/// is gone: signal a clean end.
async fn send_activity_frame(
    socket: &mut WebSocket,
    event: aion_core::ActivityEvent,
) -> Result<std::ops::ControlFlow<()>, ServerError> {
    let frame = encode_activity_frame(&event)?;
    if socket.send(Message::Text(frame.into())).await.is_err() {
        return Ok(std::ops::ControlFlow::Break(()));
    }
    Ok(std::ops::ControlFlow::Continue(()))
}

/// Serialize + send one live transcript event on the write half of the split
/// socket. A serialize failure sends a terminal wire-error frame and surfaces the
/// error; a send failure is a benign client-gone end.
async fn forward_live_frame<Tx>(
    socket_tx: &mut Tx,
    event: aion_core::ActivityEvent,
) -> Result<std::ops::ControlFlow<()>, ServerError>
where
    Tx: futures::Sink<Message> + Unpin,
    <Tx as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    let frame = match encode_activity_frame(&event) {
        Ok(frame) => frame,
        Err(error) => {
            super::socket::send_wire_error(socket_tx, &error.to_wire_error()).await?;
            return Err(error);
        }
    };
    if socket_tx.send(Message::Text(frame.into())).await.is_err() {
        return Ok(std::ops::ControlFlow::Break(()));
    }
    Ok(std::ops::ControlFlow::Continue(()))
}

fn encode_activity_frame(event: &aion_core::ActivityEvent) -> Result<String, ServerError> {
    let frame = StreamedActivityEvent::new(event.clone());
    serde_json::to_string(&frame).map_err(|source| ServerError::Wire {
        wire: WireError::backend(format!(
            "failed to serialize transcript event frame: {source}"
        )),
    })
}

/// Send the typed `transcript_lagged` terminal frame + close, then surface it
/// typed — the client re-resumes from the durable `O` tail by `store_seq`.
async fn deliver_transcript_terminal<Tx>(
    socket_tx: &mut Tx,
    skipped: u64,
) -> Result<(), ServerError>
where
    Tx: futures::Sink<Message> + Unpin,
    <Tx as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    let payload = serde_json::json!({
        "error": { "code": "transcript_lagged", "skipped": skipped },
    });
    let payload = serde_json::to_string(&payload).map_err(|source| ServerError::Wire {
        wire: WireError::backend(format!(
            "failed to serialize transcript lag frame: {source}"
        )),
    })?;
    if socket_tx.send(Message::Text(payload.into())).await.is_ok() {
        let close = CloseFrame {
            code: close_code::ERROR,
            reason: "transcript_lagged".into(),
        };
        let close_result = socket_tx.send(Message::Close(Some(close))).await;
        drop(close_result);
    }
    Err(ServerError::lagged_stream())
}

/// Finish a graceful transcript subscription end with a close-1000 frame.
async fn send_normal_close<Tx>(socket_tx: &mut Tx) -> Result<(), ServerError>
where
    Tx: futures::Sink<Message> + Unpin,
    <Tx as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    let close = CloseFrame {
        code: close_code::NORMAL,
        reason: "subscription complete".into(),
    };
    let close_result = socket_tx.send(Message::Close(Some(close))).await;
    drop(close_result);
    Ok(())
}
