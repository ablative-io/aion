//! Workflow query service types.

/// Concrete delegated query service over the engine's query mailbox seam.
pub mod concrete;
/// Query service errors, results, and engine-facing implementation.
pub mod service;

pub use concrete::ConcreteQueryService;
pub use service::{QueryError, QueryResult, QueryService, QueryServiceResult};
