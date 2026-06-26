//! Durable libSQL-backed event-store implementation for Aion workflows.
//!
//! The crate opens embedded or embedded-replica libSQL databases, applies the
//! Aion schema, and implements the `aion_store` history, timer, and visibility
//! traits through `LibSqlStore`.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use aion_store_libsql::LibSqlStore;
//!
//! let store = LibSqlStore::open("aion.db").await?;
//! store.validate_event_compatibility().await?;
//! # Ok(())
//! # }
//! ```

mod append;
/// Operator-facing libSQL connection configuration.
pub mod config;
/// libSQL database and connection opening helpers.
pub mod connection;
/// Error conversion helpers for libSQL-backed storage.
pub mod error;
mod outbox;
#[cfg(test)]
#[path = "outbox_tests.rs"]
mod outbox_tests;
mod package;
mod read;
/// Idempotent schema creation for the libSQL event store.
pub mod schema;
/// `LibSqlStore` and its event-store trait implementations.
pub mod store;
mod timer;
mod visibility;

pub use config::{LibSqlConfig, LibSqlMode};
pub use outbox::OutboxRowState;
pub use store::LibSqlStore;
