//! Cluster request routing/resolution for the workflow gRPC service (R-1..R-4):
//! steered-start placement and signal/query/cancel/reopen forward-or-local
//! decisions. Compiled only under `feature = "haematite-backend"`.
//!
//! Peeled out of `grpc/mod.rs` (AO-007 500-code-line split); the methods stay
//! private on `WorkflowGrpcService` via a second `impl` block.

use tonic::Status;

use aion_proto::WireError;
use aion_proto::generated;

use super::WorkflowGrpcService;
use super::status::status_from_wire_error;

impl WorkflowGrpcService {
    /// Resolve routing for a signal/query/cancel at the edge (R-1/R-2/R-3).
    ///
    /// Returns:
    /// - [`RouteResolution::Local`] — proceed to the local engine handler. This
    ///   is the only outcome for single-node / non-clustered boots (no cluster
    ///   store), so the default path is unchanged.
    /// - [`RouteResolution::Reply`] — the request was forwarded to the owner and
    ///   this is its relayed reply.
    /// - [`RouteResolution::Reject`] — return this typed `NotOwner` status (no
    ///   forward target, hop cap exceeded, or re-resolution still off-owner).
    ///
    /// `workflow_id` is the request's (optional) proto id; a missing/malformed id
    /// is left to the handler's existing validation (routing only acts on a
    /// well-formed target). `metadata` is the inbound caller metadata — copied
    /// onto the forward so the owner authorizes identically — and carries the hop
    /// count for loop prevention. `request` is the verbatim RPC to relay.
    pub(super) async fn resolve_route(
        &self,
        workflow_id: Option<aion_proto::ProtoWorkflowId>,
        metadata: &tonic::metadata::MetadataMap,
        request: crate::routing::ForwardRequest,
    ) -> RouteResolution {
        use crate::routing::{RouteDecision, route_mutation};
        let Some(cluster_store) = self.state.cluster_store() else {
            return RouteResolution::Local;
        };
        let Some(proto) = workflow_id else {
            return RouteResolution::Local;
        };
        let Ok(workflow_id) = aion_core::WorkflowId::try_from(proto) else {
            return RouteResolution::Local;
        };
        let directory = self
            .state
            .shard_directory()
            .map(|directory| directory.as_ref() as &dyn crate::routing::ShardDirectory);
        match route_mutation(Some(cluster_store.as_ref()), directory, &workflow_id) {
            RouteDecision::Local => RouteResolution::Local,
            RouteDecision::NotOwner { shard } => RouteResolution::Reject(not_owner_status(shard)),
            RouteDecision::Forward { owner, shard } => {
                self.forward_or_reject(owner, shard, metadata, request)
                    .await
            }
        }
    }

    /// Forward a resolved non-local request to `owner`, enforcing the hop cap and
    /// returning a typed `NotOwner` rather than forwarding when the owner is not
    /// forwardable, the cap is exceeded, or the forward itself reports the target
    /// is stale (§2.5: re-resolve-or-reject discipline).
    async fn forward_or_reject(
        &self,
        owner: crate::routing::NodeRef,
        shard: usize,
        metadata: &tonic::metadata::MetadataMap,
        request: crate::routing::ForwardRequest,
    ) -> RouteResolution {
        use crate::routing::{MAX_FORWARD_HOPS, current_hops};
        // Loop prevention: a request that has already taken the maximum number of
        // hops is not forwarded again — break the chain with NotOwner so the
        // original caller re-resolves with backoff.
        if current_hops(metadata) >= MAX_FORWARD_HOPS {
            return RouteResolution::Reject(not_owner_status(shard));
        }
        // A known-but-not-forwardable owner (no declared gRPC address) → NotOwner.
        let Some(target) = owner.grpc_addr else {
            return RouteResolution::Reject(not_owner_status(shard));
        };
        let Some(forwarder) = self.state.request_forwarder() else {
            return RouteResolution::Reject(not_owner_status(shard));
        };
        match forwarder.forward(target, metadata.clone(), request).await {
            Ok(reply) => RouteResolution::Reply(reply),
            // The forward target is stale or unreachable (it may have just died /
            // not yet adopted). Return NotOwner so the caller re-resolves; under
            // the v1.5 overlay the directory then sees the target down (§2.5).
            Err(_status) => RouteResolution::Reject(not_owner_status(shard)),
        }
    }

    /// R-1 unsteered-start placement: an id re-minted onto a locally-owned shard
    /// when this clustered node owns only a subset of shards, else `None` (engine
    /// mints as usual — the default path).
    fn start_placement(&self) -> Option<aion_core::WorkflowId> {
        use crate::routing::{RemintOutcome, route_start};
        match route_start(self.state.cluster_store().map(AsRef::as_ref)) {
            RemintOutcome::UseId(workflow_id) => Some(workflow_id),
            RemintOutcome::EngineMint => None,
        }
    }

    /// Resolve a `start` at the edge (R-4 steered start over R-1 remint).
    ///
    /// With a non-empty `routing_key` on a clustered node, the target shard is
    /// derived from the key and the start is steered to its owner: forwarded when
    /// a live remote node owns the shard, run locally on a key-shard-minted id
    /// otherwise, or rejected `NotOwner` when the owner is unreachable. With no
    /// routing key (or no cluster store) this falls back to the R-1 unsteered
    /// remint — so the single-node / unsteered path is unchanged.
    pub(super) async fn resolve_start(
        &self,
        request: &generated::StartWorkflowRequest,
        metadata: &tonic::metadata::MetadataMap,
    ) -> StartResolution {
        use crate::routing::{SteerDecision, route_start_steered};
        let routing_key = request.routing_key.as_deref().filter(|key| !key.is_empty());
        let Some(routing_key) = routing_key else {
            // Unsteered: keep the R-1 remint behaviour exactly.
            return StartResolution::Local(self.start_placement());
        };
        let Some(cluster_store) = self.state.cluster_store() else {
            // A routing key on a non-clustered node has no shards to steer to:
            // let the engine mint as usual (unsteered fallback).
            return StartResolution::Local(None);
        };
        let directory = self
            .state
            .shard_directory()
            .map(|directory| directory.as_ref() as &dyn crate::routing::ShardDirectory);
        match route_start_steered(cluster_store.as_ref(), directory, routing_key) {
            SteerDecision::Local(workflow_id) => StartResolution::Local(Some(workflow_id)),
            SteerDecision::NotOwner { shard } => StartResolution::Reject(not_owner_status(shard)),
            SteerDecision::Forward { owner, shard } => {
                self.forward_or_reject_start(owner, shard, metadata, request.clone())
                    .await
            }
        }
    }

    /// Forward a steered `start` to its shard owner, enforcing the hop cap and
    /// returning `NotOwner` rather than forwarding when the owner is not
    /// forwardable, the cap is exceeded, or the forward reports a stale target
    /// (§2.5 re-resolve-or-reject — identical discipline to signal/query/cancel).
    async fn forward_or_reject_start(
        &self,
        owner: crate::routing::NodeRef,
        shard: usize,
        metadata: &tonic::metadata::MetadataMap,
        request: generated::StartWorkflowRequest,
    ) -> StartResolution {
        use crate::routing::{ForwardReply, ForwardRequest, MAX_FORWARD_HOPS, current_hops};
        if current_hops(metadata) >= MAX_FORWARD_HOPS {
            return StartResolution::Reject(not_owner_status(shard));
        }
        let Some(target) = owner.grpc_addr else {
            return StartResolution::Reject(not_owner_status(shard));
        };
        let Some(forwarder) = self.state.request_forwarder() else {
            return StartResolution::Reject(not_owner_status(shard));
        };
        match forwarder
            .forward(target, metadata.clone(), ForwardRequest::Start(request))
            .await
        {
            Ok(ForwardReply::Start(reply)) => StartResolution::Reply(reply),
            Ok(_) => {
                StartResolution::Reject(Status::internal("forwarder returned a mismatched reply"))
            }
            // Stale/unreachable target: NotOwner so the caller re-resolves (§2.5).
            Err(_status) => StartResolution::Reject(not_owner_status(shard)),
        }
    }
}

/// The edge's routing outcome for a `start` (R-1 remint / R-4 steered start).
pub(super) enum StartResolution {
    /// Run the start locally with this placement id (`None` → engine mints).
    Local(Option<aion_core::WorkflowId>),
    /// The steered start was forwarded; relay this reply to the caller.
    Reply(generated::StartWorkflowResponse),
    /// Return this typed status (`NotOwner` / internal) to the caller.
    Reject(Status),
}

/// The edge's routing outcome for a signal/query/cancel (R-3).
pub(super) enum RouteResolution {
    /// Proceed to the local engine handler.
    Local,
    /// The request was forwarded; relay this reply to the caller.
    Reply(crate::routing::ForwardReply),
    /// Return this typed `NotOwner` status to the caller.
    Reject(Status),
}

/// Build the typed retryable `NotOwner` tonic status for shard `shard` (R-1).
fn not_owner_status(shard: usize) -> Status {
    let wire = WireError::not_owner(format!(
        "workflow shard {shard} is owned by another cluster node"
    ))
    .with_error_type("NotOwner");
    status_from_wire_error(wire)
}
