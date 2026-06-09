//! Workflow and activity supervision policies.

/// Spawn and cancellation policies for supervised workflows and activities.
pub mod policy;
/// Supervision tree identifiers and topology records.
pub mod tree;

pub use policy::{
    cancel_workflow_by_link_propagation, spawn_activity_with_policy, spawn_workflow_with_policy,
};
pub use tree::{
    EngineSupervisorId, SupervisionTree, TypeSupervisorId, TypeSupervisorNode, WorkflowNode,
};
