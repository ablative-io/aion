//! Pure domain model: `Event` enum, `Payload`, identifiers, `WorkflowStatus`, filters, error taxonomy. The vocabulary every other component speaks. Leaf crate.

pub mod error;
pub mod event;
pub mod ids;
pub mod payload;

pub use error::{ActivityError, ActivityErrorKind, WorkflowError};
pub use event::{Event, EventEnvelope};
pub use ids::{ActivityId, IdError, RunId, TimerId, WorkflowId};
pub use payload::{ContentType, Payload, PayloadError};
