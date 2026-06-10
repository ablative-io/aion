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
/// Type-erased payload bytes with explicit content-type metadata.
pub mod payload;
/// Schedule configuration, trigger, and catch-up policy models.
pub mod schedule;
/// Search-attribute schemas and values used by visibility queries.
pub mod search;
/// Workflow lifecycle status derivation.
pub mod status;

pub use error::{ActivityError, ActivityErrorKind, WorkflowError};
pub use event::{Event, EventEnvelope, WithTimeoutOutcome};
pub use filter::{WorkflowFilter, WorkflowSummary};
pub use ids::{ActivityId, IdError, RunId, TimerId, WorkflowId};
pub use payload::{ContentType, Payload, PayloadError};
pub use schedule::{CatchUpPolicy, OverlapPolicy, ScheduleConfig, ScheduleId, TriggerSpec};
pub use search::{
    SearchAttributeError, SearchAttributeSchema, SearchAttributeType, SearchAttributeValue,
};
pub use status::{WorkflowStatus, status_from_events};
