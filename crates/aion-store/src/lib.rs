//! Persistence contract for Aion event stores. Leaf crate depending only on `aion-core`.

pub mod conformance;
pub mod error;
pub mod memory;
pub mod run_chain;
pub mod store;
pub mod timer;
pub mod visibility;

pub use aion_core::{
    ContentType, Event, EventEnvelope, Payload, TimerId, WorkflowError, WorkflowFilter, WorkflowId,
    WorkflowStatus, WorkflowSummary, status_from_events,
};
pub use error::StoreError;
pub use memory::InMemoryStore;
pub use store::{EventStore, RunSummary};
pub use timer::TimerEntry;
pub use visibility::{
    ListWorkflowsFilter, SearchAttributePredicate, VisibilityRecord, VisibilityStore,
    WorkflowSummary as VisibilityWorkflowSummary,
};
