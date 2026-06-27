//! Single-node [`haematite`]-backed implementation of Aion's `EventStore`.
//!
//! [`HaematiteStore`] persists Aion workflow history, durable timers, deployed
//! packages, package routes, and the durable outbox on top of a single-node
//! [`haematite::Database`], satisfying the same `aion_store::EventStore` contract
//! the in-memory and libSQL stores satisfy. See [`store`] for the design and the
//! event/KV key-encoding scheme.
//!
//! [`HaematiteStore`] runs in one of two modes. In **single-node** mode
//! ([`HaematiteStore::create`]/[`HaematiteStore::open`]) every write is a local
//! haematite commit (B1). In **distributed** mode
//! ([`HaematiteStore::with_distribution`]) event appends are quorum-REPLICATED to
//! a cluster membership over haematite's `replicate_append`, so a workflow's
//! durable history survives the owner node's death and is readable on the
//! survivor once it becomes the shard owner (B2). Workflows are enumerated from
//! the replicated event streams themselves, so there is no separate workflow-id
//! index. The outbox stays Design-B local and is rebuilt from the replicated
//! history on the survivor.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use aion_store::{ReadableEventStore, WorkflowId};
//! use aion_store_haematite::HaematiteStore;
//!
//! let store = HaematiteStore::create("/tmp/aion-haematite-store")?;
//! let workflow_id = WorkflowId::new_v4();
//! let history = store.read_history(&workflow_id).await?;
//! assert!(history.is_empty());
//! # Ok(())
//! # }
//! ```

mod error;
mod keyspace;
/// The [`HaematiteStore`] type and its `EventStore` trait implementations.
pub mod store;

pub use store::{ClusterBootstrap, ClusterResponder, HaematiteStore};
