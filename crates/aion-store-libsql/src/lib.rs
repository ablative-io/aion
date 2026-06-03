//! The default durable `EventStore` over libSQL. Embedded local file plus embedded-replica sync for the distributed path. Runs the `aion-store` conformance suite.

pub mod append;
pub mod config;
pub mod connection;
pub mod error;
pub mod read;
pub mod schema;
pub mod store;
pub mod timer;
