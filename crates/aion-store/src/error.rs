//! `StoreError` taxonomy.

use aion_core::WorkflowId;

/// Errors returned by [`crate::ReadableEventStore`] and [`crate::EventStore`] implementations.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The workflow history head did not match the caller's optimistic-concurrency guard.
    #[error(
        "sequence conflict (double-writer bug indicator): expected workflow head {expected}, found {found}"
    )]
    SequenceConflict {
        /// Sequence number the caller expected to be the current workflow head.
        expected: u64,
        /// Sequence number currently stored as the workflow head.
        found: u64,
    },

    /// Reserved for operations that target a must-exist workflow; read and query methods return
    /// empty results, not `NotFound`, for absent workflows.
    #[error("workflow {workflow_id} was not found")]
    NotFound {
        /// Workflow identifier targeted by the must-exist operation.
        workflow_id: WorkflowId,
    },

    /// The targeted shard is owned by a different node: a quorum write was fenced
    /// because this node is not the current owner of the workflow's shard. Unlike
    /// [`Self::Backend`] this is a typed, *retryable* routing signal — the caller
    /// (or the request-routing edge) should re-resolve the shard's owner and retry
    /// or forward, rather than treating it as an opaque internal failure. Emitted
    /// only on the distributed (`[store.cluster]`) replication path.
    #[error("shard {shard} is owned by another node (not owner)")]
    NotOwner {
        /// The distribution shard the fenced workflow's durable state lives on.
        shard: usize,
    },

    /// Backend-specific failure mapped into the store contract's closed error surface.
    #[error("store backend error: {0}")]
    Backend(String),

    /// Serialization or deserialization failure while crossing the store boundary.
    #[error("store serialization error: {0}")]
    Serialization(String),
}
