//! Module declarations and re-exports.

pub mod guard;
pub mod resolver;

pub use guard::{NamespaceGuard, NamespaceOperation, SubscriptionScope, WorkflowTarget};
pub use resolver::{CallerIdentity, NamespaceResolver, ScopedEngine, WorkflowOwnership};
