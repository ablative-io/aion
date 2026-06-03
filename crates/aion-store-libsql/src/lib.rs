//! Durable `EventStore` support backed by libSQL.

pub mod config;
pub mod connection;
pub mod error;
pub mod schema;

pub use config::{LibSqlConfig, LibSqlMode};
