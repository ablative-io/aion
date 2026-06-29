//! Namespace authorization and routing support.

/// Namespace guard rules for workflow operations and subscriptions.
pub mod guard;
/// Caller identity resolution and scoped-engine selection.
pub mod resolver;
/// Durable schedule→namespace ownership sources.
pub mod schedule_source;

pub use guard::{
    NamespaceGuard, NamespaceOperation, ScheduleTarget, SubscriptionScope, WorkflowTarget,
};
pub use resolver::{
    CallerIdentity, NAMESPACE_ATTRIBUTE, NamespaceResolver, ScopedEngine, StaticWorkflowNamespaces,
    TASK_QUEUE_ATTRIBUTE, WorkflowAttribution, WorkflowNamespaceSource,
};
pub use schedule_source::{ScheduleNamespaceSource, StaticScheduleNamespaces};
