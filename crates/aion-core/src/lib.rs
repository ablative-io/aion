//! Pure domain model: `Event` enum, `Payload`, identifiers, `WorkflowStatus`, filters, error taxonomy. The vocabulary every other component speaks. Leaf crate.

pub mod error;
pub mod event;
pub mod filter;
#[cfg(test)]
mod generated_types;
pub mod ids;
pub mod payload;
pub mod schedule;
pub mod status;

pub use error::{ActivityError, ActivityErrorKind, WorkflowError};
pub use event::{Event, EventEnvelope};
pub use filter::{WorkflowFilter, WorkflowSummary};
pub use ids::{ActivityId, IdError, RunId, TimerId, WorkflowId};
pub use payload::{ContentType, Payload, PayloadError};
pub use schedule::{CatchUpPolicy, OverlapPolicy, ScheduleConfig, ScheduleId, TriggerSpec};
pub use status::{WorkflowStatus, status_from_events};
