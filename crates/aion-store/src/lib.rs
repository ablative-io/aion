//! Persistence contract for Aion event stores. Leaf crate depending only on `aion-core`.

pub mod conformance;
pub mod error;
pub mod memory;
pub mod store;
pub mod timer;

pub use aion_core::{
    ContentType, Event, EventEnvelope, Payload, TimerId, WorkflowError, WorkflowFilter, WorkflowId,
    WorkflowStatus, WorkflowSummary, status_from_events,
};
pub use error::StoreError;
pub use memory::InMemoryStore;
pub use store::EventStore;
pub use timer::TimerEntry;
