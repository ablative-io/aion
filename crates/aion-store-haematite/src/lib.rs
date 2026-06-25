//! Single-node [`haematite`]-backed implementation of Aion's `EventStore`.
//!
//! [`HaematiteStore`] persists Aion workflow history, durable timers, deployed
//! packages, package routes, and the durable outbox on top of a single-node
//! [`haematite::Database`], satisfying the same `aion_store::EventStore` contract
//! the in-memory and libSQL stores satisfy. See [`store`] for the design and the
//! event/KV key-encoding scheme.
//!
//! Replication and multi-node failover are intentionally out of scope for this
//! increment (B1): the store runs haematite with a single shard and no
//! distribution. A later increment builds the cluster failover path on the
//! replicated haematite substrate.
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

pub use store::HaematiteStore;
