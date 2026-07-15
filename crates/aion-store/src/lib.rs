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
/// Durable, minted-on-use namespace registry records and contract.
pub mod namespace;
/// Durable observability (`O`) keyspace contract — the NOI-5 transcript spine.
pub mod observability;
/// Durable outbox contract for store-backed fan-out dispatch.
pub mod outbox;
/// Deployed-package persistence records and contract.
pub mod package;
/// Run-chain summaries used for workflow execution lineage.
pub mod run_chain;
/// Core readable and writable event-store traits.
pub mod store;
/// Recording test doubles for store decorators, gated behind `test-support`.
#[cfg(feature = "test-support")]
pub mod testing;
/// Timer persistence records and queries.
pub mod timer;
/// Workflow visibility records, predicates, and list filters.
pub mod visibility;

pub use aion_core::{
    ContentType, Event, EventEnvelope, Payload, RunId, TimerId, WorkflowError, WorkflowFilter,
    WorkflowId, WorkflowStatus, WorkflowSummary, status_from_events,
};
pub use error::StoreError;
pub use memory::InMemoryStore;
pub use namespace::{
    MintOutcome, NamespaceConfig, NamespaceOrigin, NamespacePlacement, NamespaceRecord,
    NamespaceState, NamespaceStore,
};
pub use observability::{
    ActivityRecord, ActivityStreamKey, ActivityStreamSummary, InMemoryObservabilityStore,
    ObservabilityStore,
};
pub use outbox::{ClaimScope, DEFAULT_OUTBOX_ROUTE, OutboxRow, OutboxStatus, OutboxStore};
pub use package::{PackageRecord, PackageRouteRecord, PackageStore};
pub use store::{EventStore, ReadableEventStore, RunSummary, WritableEventStore, WriteToken};
pub use timer::TimerEntry;
pub use visibility::{
    ListWorkflowsFilter, SearchAttributePredicate, VisibilityRecord, VisibilityStore,
    WorkflowSummary as VisibilityWorkflowSummary,
};
