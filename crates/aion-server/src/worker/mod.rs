//! Module declarations.

pub mod bridge;
pub mod dispatch;
pub mod heartbeat;
pub mod registry;

pub use bridge::{PendingActivities, WorkerActivityDispatcher};
pub use dispatch::{
    ActivityCompletion, ActivityCompletionOutcome, ActivityCompletionSink, ActivityDispatcher,
    ScheduledActivity, handle_activity_result,
};
pub use heartbeat::{
    HeartbeatTracker, HeartbeatUpdate, InFlightActivity, LostWorkerReport, TaskLiveness,
};
pub use registry::{ConnectedWorkerRegistry, WorkerHandle, WorkerId, WorkerRegistration};
