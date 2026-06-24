//! Module declarations.

/// Bridge from engine activity dispatch to connected workers.
pub mod bridge;
/// Activity completion handling and dispatch abstractions.
pub mod dispatch;
/// Worker heartbeat and liveness tracking.
pub mod heartbeat;
/// Non-replayed durable-outbox fan-out dispatcher (dormant unless commissioned).
pub mod outbox_dispatcher;
/// Connected-worker registry and handles.
pub mod registry;

pub use bridge::{PendingActivities, WorkerActivityDispatcher};
pub use dispatch::{
    ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink, ActivityDispatcher,
    ScheduledActivity, handle_activity_result,
};
pub use heartbeat::{
    HeartbeatTracker, HeartbeatUpdate, InFlightActivity, LostWorkerReport, TaskLiveness,
};
pub use outbox_dispatcher::{
    OutboxDispatcher, OutboxDispatcherConfig, OutboxRowDispatch, WorkerOutboxDispatch,
};
pub use registry::{ConnectedWorkerRegistry, WorkerHandle, WorkerId, WorkerRegistration};
