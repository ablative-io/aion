//! Durable `EventStore` support backed by libSQL.

pub mod config;
pub mod error;

pub use config::{LibSqlConfig, LibSqlMode};
