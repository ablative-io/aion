//! Persistence contracts and in-memory event stores for Aion durable workflows.
//!
//! The crate defines the async event-store traits used by the engine, visibility
//! records for workflow listings, timer records, run-chain summaries, and a
//! correct `InMemoryStore` reference implementation for tests and development.
//!
//! # Example
//!
//! ```
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use aion_store::{InMemoryStore, ReadableEventStore, WorkflowId};
//!
//! let store = InMemoryStore::default();
//! let workflow_id = WorkflowId::new_v4();
//! let history = store.read_history(&workflow_id).await?;
//! assert!(history.is_empty());
//! # Ok(())
//! # }
//! ```

/// Backend conformance helpers shared by event-store implementations.
pub mod conformance;
/// Store-level error taxonomy.
pub mod error;
/// In-memory reference implementation of the store contracts.
pub mod memory;
/// Run-chain summaries used for workflow execution lineage.
pub mod run_chain;
/// Core readable and writable event-store traits.
pub mod store;
/// Timer persistence records and queries.
pub mod timer;
/// Workflow visibility records, predicates, and list filters.
pub mod visibility;

pub use aion_core::{
    ContentType, Event, EventEnvelope, Payload, TimerId, WorkflowError, WorkflowFilter, WorkflowId,
    WorkflowStatus, WorkflowSummary, status_from_events,
};
pub use error::StoreError;
pub use memory::InMemoryStore;
pub use store::{EventStore, ReadableEventStore, RunSummary, WritableEventStore, WriteToken};
pub use timer::TimerEntry;
pub use visibility::{
    ListWorkflowsFilter, SearchAttributePredicate, VisibilityRecord, VisibilityStore,
    WorkflowSummary as VisibilityWorkflowSummary,
};
