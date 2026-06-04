//! Durable `EventStore` support backed by libSQL.

mod append;
pub mod config;
pub mod connection;
pub mod error;
mod read;
pub mod schema;
pub mod store;
mod timer;

pub use config::{LibSqlConfig, LibSqlMode};
pub use store::LibSqlStore;
