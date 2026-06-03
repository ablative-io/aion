//! Pure domain model: `Event` enum, `Payload`, identifiers, `WorkflowStatus`, filters, error taxonomy. The vocabulary every other component speaks. Leaf crate.

pub mod ids;
pub mod payload;

pub use ids::{ActivityId, RunId, TimerId, WorkflowId};
pub use payload::{ContentType, Payload, PayloadError};
