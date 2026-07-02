//! Module declarations.

/// Per-tenant keyed backpressure at the outbox claim (Control-Plane Phase 2,
/// P2-Q2).
pub mod backpressure;
/// Bridge from engine activity dispatch to connected workers.
pub mod bridge;
/// Activity completion handling and dispatch abstractions.
pub mod dispatch;
/// Worker heartbeat and liveness tracking.
pub mod heartbeat;
/// Server-side mid-run intervention routing (NOI-6): capability gate + attempt
/// owner resolution + push to the owning worker over a pluggable transport.
pub mod intervention;
/// Cross-node outbox dispatch over the liminal bus (#13-0 spike, feature-gated).
#[cfg(feature = "liminal-transport")]
pub mod liminal_transport;
/// Server-side outbox completion delivery into live workflows.
pub mod outbox_delivery;
/// Non-replayed durable-outbox fan-out dispatcher (dormant unless commissioned).
pub mod outbox_dispatcher;
/// Live stale-claim outbox reconciler (dormant unless commissioned).
pub mod outbox_reconciler;
/// Short-TTL per-namespace placement cache for the dispatcher (Control-Plane
/// Phase 2, P2-P3).
pub mod placement_cache;
/// Throttled per-namespace quota-state broadcaster for the ops-console live badge
/// (Control-Plane Phase 2, P2-Q3).
pub mod quota_broadcast;
/// Short-TTL per-namespace concurrency-quota cache for the dispatcher's keyed
/// backpressure (Control-Plane Phase 2, P2-Q2).
pub mod quota_cache;
/// Connected-worker registry and handles.
pub mod registry;

pub use backpressure::{Backpressure, OwnedShardFraction};
pub use bridge::{OutboxDeliveryCallback, PendingActivities, WorkerActivityDispatcher};
pub use dispatch::{
    ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink, ActivityDispatcher,
    ScheduledActivity, handle_activity_result,
};
pub use heartbeat::{
    HeartbeatSweeper, HeartbeatTracker, HeartbeatUpdate, InFlightActivity, LostWorkerReport,
    TaskLiveness, sweep_interval,
};
pub use intervention::{AttemptKey, AttemptOwnerIndex, InterventionRouter, InterventionTransport};
#[cfg(feature = "liminal-transport")]
pub use liminal_transport::{
    DispatchRequest, DispatchResponse, InterventionReply, InterventionRequest,
    LiminalCompletionSource, LiminalConnectionNotifier, LiminalInterventionTransport,
    LiminalWorkerDelivery, RegistryLiminalDispatch, channel_for_row, dispatch_channel_name,
};
pub use outbox_delivery::ServerOutboxDeliveryCallback;
pub use outbox_dispatcher::{
    OutboxDispatcher, OutboxDispatcherConfig, OutboxRowDispatch, WorkerOutboxDispatch,
};
pub use outbox_reconciler::{OutboxReconciler, OutboxReconcilerConfig};
pub use placement_cache::{
    PlacementCache, WorkerSelection, preferred_node_order, worker_selection_for,
};
pub use quota_broadcast::QuotaBroadcaster;
pub use quota_cache::QuotaCache;
pub use registry::{
    ConnectedWorkerRegistry, WorkerDelivery, WorkerHandle, WorkerId, WorkerRegistration,
};
