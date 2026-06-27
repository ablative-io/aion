//! Distributed request routing — the gRPC-edge availability layer over the
//! epoch fence (DISTRIBUTED-ROUTING-DESIGN §2).
//!
//! A node that is not the current owner of `shard_for(workflow_id)` cannot win
//! the quorum CAS for that workflow's writes: the owner's promised ballot fences
//! the proposal (surfaced as [`aion_store::StoreError::NotOwner`], R-0). Routing
//! turns that correctness backstop into an *availability* layer: at the edge we
//! resolve the shard owner and either proceed locally, forward to the owner
//! (R-3), or return the typed retryable `NotOwner` so the caller re-resolves.
//!
//! This module is behind the `haematite-backend` feature and only ever does
//! anything for a distributed (`[store.cluster]`) boot. With no cluster store on
//! [`crate::ServerState`] every entry point short-circuits to "proceed locally",
//! so the default single-node path is byte-identical — the fence is never even
//! reachable there.
//!
//! ## Staging
//!
//! - **R-1 (here):** a `shard_for`-aware edge guard for signal/query/cancel on a
//!   non-owned shard (return `NotOwner`), plus an unsteered-start remint so a
//!   `start` whose default-minted id would land off-owner is re-minted onto a
//!   locally-owned shard and never fences (§2.4 stopgap).
//! - **R-2:** a `ShardDirectory` trait + static resolver so the edge knows the
//!   *remote* owner's gRPC address, not just "not me".
//! - **R-3:** a `RequestForwarder` trait + gRPC forwarder that relays a non-local
//!   request to the owner instead of rejecting it.

#[cfg(feature = "haematite-backend")]
mod directory;
#[cfg(feature = "haematite-backend")]
mod edge;
#[cfg(feature = "haematite-backend")]
mod forwarder;

#[cfg(feature = "haematite-backend")]
pub use directory::{DirectoryPeer, NodeRef, OwnerView, ShardDirectory, StaticShardDirectory};
#[cfg(feature = "haematite-backend")]
pub use edge::{RemintOutcome, RouteDecision, route_mutation, route_start};
#[cfg(feature = "haematite-backend")]
pub use forwarder::{
    FORWARD_HOPS_METADATA, ForwardReply, ForwardRequest, GrpcRequestForwarder, MAX_FORWARD_HOPS,
    RequestForwarder, current_hops,
};
