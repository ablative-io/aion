//! Workflow query service types.

/// Query service errors, results, and engine-facing implementation.
pub mod service;

pub use service::{QueryError, QueryResult, QueryService, QueryServiceResult};
