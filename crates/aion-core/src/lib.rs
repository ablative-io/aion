//! Pure domain model: Event enum, Payload, identifiers, WorkflowStatus, filters, error taxonomy. The vocabulary every other component speaks. Leaf crate.

pub mod error;
pub mod event;
pub mod filter;
pub mod ids;
pub mod payload;
pub mod status;
