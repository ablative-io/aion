//! Pure domain model and shared vocabulary for Aion durable workflows.
//!
//! This leaf crate defines the stable identifiers, payload carrier, history events,
//! workflow filters, schedule settings, search attributes, statuses, and error
//! taxonomy used by every other Aion component.
//!
//! # Example
//!
//! ```
//! use aion_core::{Payload, WorkflowId};
//! use serde_json::json;
//!
//! let workflow_id = WorkflowId::new_v4();
//! let payload = Payload::from_json(&json!({ "workflow_id": workflow_id.to_string() }))?;
//! assert_eq!(payload.to_json()?["workflow_id"], workflow_id.to_string());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

/// Agent-observability transcript events for the ops console real-time channel.
pub mod activity_event;
/// Cluster topology and ownership events for the ops console real-time channel (WS3).
pub mod cluster_event;
/// Describe-workflow response projection (summary + event history).
pub mod describe;
/// Error types shared by workflow engines, callers, and activities.
pub mod error;
/// Durable workflow history events and envelopes.
pub mod event;
/// Workflow visibility filters and summaries.
pub mod filter;
#[cfg(test)]
mod generated_types;
/// Strongly typed identifiers for workflows, runs, activities, timers, and schedules.
pub mod ids;
/// Harness-neutral mid-run intervention commands for the ops console control channel.
pub mod intervention;
/// Type-erased payload bytes with explicit content-type metadata.
pub mod payload;
/// Schedule configuration, trigger, and catch-up policy models.
pub mod schedule;
/// Search-attribute schemas and values used by visibility queries.
pub mod search;
/// Workflow lifecycle status derivation.
pub mod status;

pub use activity_event::{ActivityEvent, ActivityEventKind, MessageRole, ProgressDetail, StopKind};
pub use cluster_event::{
    ClusterCommand, ClusterEvent, ClusterEventMeta, ClusterPeer, ClusterShard, ClusterSnapshot,
    ClusterStreamError, ClusterWorker, NamespacePlacementWire, WorkerDeathReason, WorkerTransport,
};
pub use describe::DescribeWorkflowResponse;
pub use error::{ActivityError, ActivityErrorKind, WorkflowError};
pub use event::{
    DEFAULT_TASK_QUEUE, Event, EventEnvelope, START_TIME_TASK_QUEUE_ATTRIBUTE, WithTimeoutOutcome,
    start_time_task_queue,
};
pub use filter::{WorkflowFilter, WorkflowSummary, failure_projection};
pub use ids::{ActivityId, IdError, PackageVersion, RunId, TimerId, TimerIdKind, WorkflowId};
pub use intervention::{
    ApprovalDecision, InjectPriority, InterventionCapabilities, InterventionCommand,
    InterventionKind, InterventionOutcome, InterventionPrimitive,
};
pub use payload::{ContentType, Payload, PayloadError};
pub use schedule::{CatchUpPolicy, OverlapPolicy, ScheduleConfig, ScheduleId, TriggerSpec};
pub use search::{
    SearchAttributeError, SearchAttributeSchema, SearchAttributeType, SearchAttributeValue,
    search_attributes_from_events,
};
pub use status::{WorkflowStatus, current_lease_terminal, run_segment, status_from_events};
