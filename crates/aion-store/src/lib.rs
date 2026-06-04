//! Persistence contract for Aion event stores. Leaf crate depending only on `aion-core`.

pub mod conformance;
pub mod error;
pub mod memory;
pub mod store;
pub mod timer;

pub use aion_core::{Event, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
pub use error::StoreError;
pub use memory::InMemoryStore;
pub use store::EventStore;
pub use timer::TimerEntry;
