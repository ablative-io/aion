//! R-3 request forwarding: the `RequestForwarder` trait + a gRPC implementation
//! that relays a non-local signal/query/cancel to the shard's current owner and
//! returns its reply (DISTRIBUTED-ROUTING-DESIGN §2.2 / §2.6 / Decision C).
//!
//! The trait mirrors the `OutboxRowDispatch` gRPC/liminal seam: a gRPC forwarder
//! ships now (a tonic `WorkflowService` client to the owner's `grpc_address`); a
//! liminal forwarder (`request_reply_conversation()`) drops in behind the same
//! trait when liminal 13-L0/L1 land (R-6). Loop prevention is a hop cap carried
//! in request metadata; stale-target handling is bounded re-resolution at the
//! edge (§2.5).

use std::net::SocketAddr;

use async_trait::async_trait;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

use aion_proto::generated::{self, workflow_service_client::WorkflowServiceClient};

/// The metadata key carrying the number of cluster hops a forwarded request has
/// already taken. A request arriving with a value at or above the hop cap is NOT
/// forwarded again (loop prevention) — the receiver returns `NotOwner` so the
/// original caller re-resolves and retries with backoff.
pub const FORWARD_HOPS_METADATA: &str = "x-aion-forward-hops";

/// The maximum number of intra-cluster forward hops a single client request may
/// take before the chain is broken with `NotOwner` (§2.5: "a hop counter caps
/// forwards (e.g. 2)").
pub const MAX_FORWARD_HOPS: u32 = 2;

/// The forwardable client RPCs whose target `workflow_id` is known at the edge.
/// `start` is excluded: its placement is handled by the R-1 remint until steered
/// start (R-4) lands.
#[derive(Clone, Debug)]
pub enum ForwardRequest {
    /// A `signal` to relay verbatim to the owner.
    Signal(generated::SignalRequest),
    /// A `query` to relay verbatim to the owner.
    Query(generated::QueryRequest),
    /// A `cancel` to relay verbatim to the owner.
    Cancel(generated::CancelRequest),
}

/// The owner's reply, relayed back to the original caller unchanged.
#[derive(Clone, Debug)]
pub enum ForwardReply {
    /// The owner's `signal` reply.
    Signal(generated::SignalResponse),
    /// The owner's `query` reply.
    Query(generated::QueryResponse),
    /// The owner's `cancel` reply.
    Cancel(generated::CancelResponse),
}

/// Relays a client request to a remote shard owner and returns its reply.
///
/// The transport is abstracted (Decision C): gRPC now, liminal later, behind one
/// trait. Errors are returned as a tonic [`Status`] so the edge can relay them
/// verbatim (an owner `NotOwner` stays `NotOwner`; a transport failure surfaces
/// as `Unavailable`).
#[async_trait]
pub trait RequestForwarder: Send + Sync {
    /// Forward `request` to `target` (the owner's gRPC address), copying the
    /// caller `metadata` and stamping the next hop count, then relay the reply.
    async fn forward(
        &self,
        target: SocketAddr,
        metadata: tonic::metadata::MetadataMap,
        request: ForwardRequest,
    ) -> Result<ForwardReply, Status>;
}

/// gRPC forwarder: dials the owner's `grpc_address` with a tonic
/// `WorkflowService` client and re-issues the RPC.
#[derive(Clone, Default)]
pub struct GrpcRequestForwarder;

impl GrpcRequestForwarder {
    /// A new gRPC forwarder.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Read the current hop count from request metadata (`0` when absent/malformed).
#[must_use]
pub fn current_hops(metadata: &tonic::metadata::MetadataMap) -> u32 {
    metadata
        .get(FORWARD_HOPS_METADATA)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// Stamp `metadata` with the incremented hop count for the outbound forward.
fn stamp_next_hop(metadata: &mut tonic::metadata::MetadataMap) -> Result<(), Status> {
    let next = current_hops(metadata)
        .checked_add(1)
        .ok_or_else(|| Status::internal("forward hop counter overflow"))?;
    let value = tonic::metadata::MetadataValue::try_from(next.to_string())
        .map_err(|_| Status::internal("invalid forward hop metadata value"))?;
    metadata.insert(FORWARD_HOPS_METADATA, value);
    Ok(())
}

async fn connect(target: SocketAddr) -> Result<WorkflowServiceClient<Channel>, Status> {
    let uri = format!("http://{target}");
    let endpoint = Endpoint::try_from(uri)
        .map_err(|error| Status::unavailable(format!("invalid forward target: {error}")))?;
    let channel = endpoint
        .connect()
        .await
        .map_err(|error| Status::unavailable(format!("forward dial failed: {error}")))?;
    Ok(WorkflowServiceClient::new(channel))
}

#[async_trait]
impl RequestForwarder for GrpcRequestForwarder {
    async fn forward(
        &self,
        target: SocketAddr,
        mut metadata: tonic::metadata::MetadataMap,
        request: ForwardRequest,
    ) -> Result<ForwardReply, Status> {
        stamp_next_hop(&mut metadata)?;
        let mut client = connect(target).await?;
        match request {
            ForwardRequest::Signal(message) => {
                let mut outbound = Request::new(message);
                *outbound.metadata_mut() = metadata;
                client
                    .signal(outbound)
                    .await
                    .map(|response| ForwardReply::Signal(response.into_inner()))
            }
            ForwardRequest::Query(message) => {
                let mut outbound = Request::new(message);
                *outbound.metadata_mut() = metadata;
                client
                    .query(outbound)
                    .await
                    .map(|response| ForwardReply::Query(response.into_inner()))
            }
            ForwardRequest::Cancel(message) => {
                let mut outbound = Request::new(message);
                *outbound.metadata_mut() = metadata;
                client
                    .cancel(outbound)
                    .await
                    .map(|response| ForwardReply::Cancel(response.into_inner()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FORWARD_HOPS_METADATA, MAX_FORWARD_HOPS, current_hops, stamp_next_hop};

    #[test]
    fn current_hops_defaults_to_zero_when_absent() {
        let metadata = tonic::metadata::MetadataMap::new();
        assert_eq!(current_hops(&metadata), 0);
    }

    #[test]
    fn stamp_next_hop_increments_from_zero() -> Result<(), tonic::Status> {
        let mut metadata = tonic::metadata::MetadataMap::new();
        stamp_next_hop(&mut metadata)?;
        assert_eq!(current_hops(&metadata), 1);
        stamp_next_hop(&mut metadata)?;
        assert_eq!(current_hops(&metadata), 2);
        Ok(())
    }

    #[test]
    fn malformed_hop_value_reads_as_zero() -> Result<(), tonic::Status> {
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert(
            FORWARD_HOPS_METADATA,
            tonic::metadata::MetadataValue::try_from("not-a-number")
                .map_err(|_| tonic::Status::internal("bad fixture"))?,
        );
        assert_eq!(current_hops(&metadata), 0);
        Ok(())
    }

    #[test]
    fn hop_cap_is_two() {
        assert_eq!(MAX_FORWARD_HOPS, 2);
    }
}
