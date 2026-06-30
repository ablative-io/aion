//! Module declarations.

/// Bridge from engine activity dispatch to connected workers.
pub mod bridge;
/// Activity completion handling and dispatch abstractions.
pub mod dispatch;
/// Worker heartbeat and liveness tracking.
pub mod heartbeat;
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
/// Connected-worker registry and handles.
pub mod registry;

pub use bridge::{OutboxDeliveryCallback, PendingActivities, WorkerActivityDispatcher};
pub use dispatch::{
    ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink, ActivityDispatcher,
    ScheduledActivity, handle_activity_result,
};
pub use heartbeat::{
    HeartbeatTracker, HeartbeatUpdate, InFlightActivity, LostWorkerReport, TaskLiveness,
};
#[cfg(feature = "liminal-transport")]
pub use liminal_transport::{
    DispatchRequest, DispatchResponse, LiminalCompletionSource, LiminalConnectionNotifier,
    LiminalWorkerDelivery, RegistryLiminalDispatch, channel_for_row, dispatch_channel_name,
};
pub use outbox_delivery::ServerOutboxDeliveryCallback;
pub use outbox_dispatcher::{
    OutboxDispatcher, OutboxDispatcherConfig, OutboxRowDispatch, WorkerOutboxDispatch,
};
pub use outbox_reconciler::{OutboxReconciler, OutboxReconcilerConfig};
pub use placement_cache::PlacementCache;
pub use registry::{
    ConnectedWorkerRegistry, WorkerDelivery, WorkerHandle, WorkerId, WorkerRegistration,
};
