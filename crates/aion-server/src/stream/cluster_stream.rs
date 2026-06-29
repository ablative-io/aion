//! WS3 cluster subscription: deploy-gated snapshot + live-delta forward loop.
//!
//! This is the cluster-channel counterpart to [`super::socket`]'s workflow
//! forward loop. It is a NEW ARM on the existing single subscription frame of
//! `/events/stream` (the socket stays one-subscription-per-socket; there is no
//! multiplexing layer). A client that wants both the workflow stream and the
//! cluster stream opens two `/events/stream` sockets.
//!
//! # Authorization: deploy-scope only (strict)
//!
//! Cluster topology is deployment-wide: peer names, shard ownership, worker
//! identities across every namespace. Exposing that to any single-namespace
//! tenant is a cross-tenant topology leak. So the cluster channel requires the
//! caller's **deploy grant** ([`CallerIdentity::deploy_granted`]) — the same
//! deployment-wide grant the deploy API uses — and nothing less. A caller
//! without it receives exactly one terminal `namespace_denied` frame then close,
//! byte-identical to the workflow path's rejection shape (no existence leak).
//!
//! Because the gate is a pure deploy-grant check it needs no engine handle (it
//! reads supervisor/registry/store state, never a namespace engine), sidestepping
//! the `guard.scope(...).engine()?` requirement the workflow path has.

use aion_core::{ClusterSnapshot, ClusterStreamError, ClusterWorker, WorkerTransport};
use aion_proto::{StreamedClusterEvent, StreamedClusterSnapshot, WireError};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{SinkExt, StreamExt};

use crate::cluster_publisher::ClusterStreamLagged;
use crate::error::ServerError;
use crate::namespace::CallerIdentity;
use crate::state::ServerState;
use crate::worker::WorkerDelivery;

/// Serve a cluster subscription on an already-upgraded socket.
///
/// Flow (mirrors `subscribe.rs` ordering): deploy-gate FIRST; then attach to the
/// live broadcast BEFORE reading the snapshot (gap-free splice — any delta that
/// races the snapshot is buffered by the receiver and deduped by the snapshot's
/// `as_of_seq`); send the priming snapshot; forward live deltas until the client
/// closes or the subscriber lags (one typed `cluster_lagged` frame then close).
///
/// # Errors
///
/// Returns [`ServerError`] when the deploy gate denies the caller (after the
/// terminal frame is sent) or the stream ends with a lag terminal frame.
pub async fn serve_cluster_socket(
    mut socket: WebSocket,
    state: &ServerState,
    caller: &CallerIdentity,
    after_seq: u64,
) -> Result<(), ServerError> {
    // GATE FIRST: deploy grant or nothing. Denial is one terminal frame + close.
    if !caller.deploy_granted() {
        let error = ServerError::namespace_denied(
            "cluster topology subscription requires the deployment-wide deploy grant",
        );
        super::socket::send_wire_error(&mut socket, &error.to_wire_error()).await?;
        return Err(error);
    }

    // T0: attach to the live broadcast BEFORE snapshotting, so a delta emitted
    // between the snapshot read and the first live poll is retained by the
    // receiver and applied after the snapshot (deduped on `cluster_seq`).
    let publisher = state.cluster_publisher();
    let mut live = publisher.subscribe(after_seq);

    // T1 (> T0): read the calm-state snapshot. `as_of_seq` is the publisher's
    // current seq; the client applies only deltas with `cluster_seq > as_of_seq`.
    let snapshot = build_snapshot(state, caller)?;
    let priming = StreamedClusterSnapshot::new(snapshot);
    let priming = serde_json::to_string(&priming).map_err(|source| ServerError::Wire {
        wire: WireError::backend(format!(
            "failed to serialize cluster snapshot frame: {source}"
        )),
    })?;
    if socket.send(Message::Text(priming.into())).await.is_err() {
        // Client gone before the priming frame landed: a clean end.
        return Ok(());
    }

    let (mut socket_tx, mut socket_rx) = socket.split();
    loop {
        tokio::select! {
            client_message = socket_rx.next() => {
                match client_message {
                    // Close, socket error, or any inbound frame ends the read
                    // side; the cluster channel takes no further client frames
                    // (one-subscription-per-socket), so an inbound frame after
                    // subscribe is treated as a benign close, exactly like the
                    // workflow `drive_socket`.
                    Some(Ok(Message::Close(_))) | None => return send_normal_close(&mut socket_tx).await,
                    Some(Ok(_other)) => {}
                    Some(Err(_error)) => return Ok(()),
                }
            }
            item = live.next() => {
                match item {
                    Some(Ok(event)) => {
                        let frame = StreamedClusterEvent::new(event);
                        let frame = match serde_json::to_string(&frame) {
                            Ok(frame) => frame,
                            Err(source) => {
                                let error = ServerError::Wire {
                                    wire: WireError::backend(format!(
                                        "failed to serialize cluster event frame: {source}"
                                    )),
                                };
                                super::socket::send_wire_error(&mut socket_tx, &error.to_wire_error()).await?;
                                return Err(error);
                            }
                        };
                        if socket_tx.send(Message::Text(frame.into())).await.is_err() {
                            return Ok(());
                        }
                    }
                    Some(Err(ClusterStreamLagged { skipped })) => {
                        // Typed terminal `cluster_lagged` frame carrying the
                        // skipped count, then close — the client re-requests a
                        // fresh snapshot (no durable cluster history to resume).
                        let lagged = ClusterStreamError::ClusterLagged { skipped };
                        return deliver_cluster_terminal(&mut socket_tx, &lagged).await;
                    }
                    None => {
                        // The publisher channel closed (server shutting down):
                        // finish the close handshake cleanly.
                        return send_normal_close(&mut socket_tx).await;
                    }
                }
            }
        }
    }
}

/// Build the calm-state snapshot: self node identity, the deploy-granted view of
/// connected workers, and (Phase 1) empty peers/shards.
///
/// Peers/shards are intentionally empty here: the supervisor's watched-peer set
/// and the shard-owner directory are only meaningful on a distributed haematite
/// boot, and surfacing them honestly requires threading that state in a later
/// increment. A single-node server has no peers and owns every shard implicitly,
/// so an empty peers/shards snapshot is the truthful calm state, not a stub. The
/// dashboard derives liveness from the live `Peer*`/`Shard*` deltas the
/// supervisor emits once wired.
pub(crate) fn build_snapshot(
    state: &ServerState,
    _caller: &CallerIdentity,
) -> Result<ClusterSnapshot, ServerError> {
    let node = state.cluster_self_node().map_or_else(
        || STANDALONE_NODE_LABEL.to_owned(),
        std::borrow::ToOwned::to_owned,
    );
    // Deploy-granted callers see every connected worker (cluster topology is
    // deployment-wide for a deploy-scoped caller). The deploy gate already ran,
    // so no per-namespace redaction applies on this strict-scope path.
    let workers = state
        .worker_registry()
        .all_workers()?
        .into_iter()
        .map(|handle| ClusterWorker {
            worker_id: handle.id().value().to_string(),
            namespaces: handle.namespaces().iter().cloned().collect(),
            task_queue: handle.task_queue().to_owned(),
            transport: transport_of(handle.delivery()),
            node: handle.node().map(str::to_owned),
        })
        .collect();
    Ok(ClusterSnapshot {
        node,
        as_of_seq: state.cluster_publisher().current_seq(),
        peers: Vec::new(),
        shards: Vec::new(),
        workers,
    })
}

/// Self-label reported as the snapshot `node` on a single-node boot that carries
/// no configured cluster distribution name.
const STANDALONE_NODE_LABEL: &str = "standalone";

/// Map a registry [`WorkerDelivery`] to the wire [`WorkerTransport`] discriminant.
const fn transport_of(delivery: &WorkerDelivery) -> WorkerTransport {
    match delivery {
        WorkerDelivery::Grpc(_) => WorkerTransport::Grpc,
        #[cfg(feature = "liminal-transport")]
        WorkerDelivery::Liminal(_) => WorkerTransport::Liminal,
    }
}

/// Send the typed cluster terminal error frame + close, then surface it typed.
async fn deliver_cluster_terminal<Tx>(
    socket_tx: &mut Tx,
    error: &ClusterStreamError,
) -> Result<(), ServerError>
where
    Tx: futures::Sink<Message> + Unpin,
    <Tx as futures::Sink<Message>>::Error: std::fmt::Debug,
{
    // The cluster terminal frame is the typed `ClusterStreamError` wrapped as
    // `{"error": ...}` — the same wrapper shape every SDK detects as terminal.
    let payload = serde_json::json!({ "error": error });
    let payload = serde_json::to_string(&payload).map_err(|source| ServerError::Wire {
        wire: WireError::backend(format!(
            "failed to serialize cluster stream error: {source}"
        )),
    })?;
    if socket_tx.send(Message::Text(payload.into())).await.is_ok() {
        let close = CloseFrame {
            code: close_code::ERROR,
            reason: "cluster_lagged".into(),
        };
        let close_result = socket_tx.send(Message::Close(Some(close))).await;
        drop(close_result);
    }
    Err(ServerError::lagged_stream())
}

/// Finish a graceful cluster subscription end with a close-1000 frame.
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
