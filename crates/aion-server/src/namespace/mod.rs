//! Namespace authorization and routing support.

/// Namespace guard rules for workflow operations and subscriptions.
pub mod guard;
/// Caller identity resolution and scoped-engine selection.
pub mod resolver;

pub use guard::{NamespaceGuard, NamespaceOperation, SubscriptionScope, WorkflowTarget};
pub use resolver::{CallerIdentity, NamespaceResolver, ScopedEngine, WorkflowOwnership};
