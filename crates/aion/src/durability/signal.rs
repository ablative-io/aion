//! Durable signal command helpers.

use aion_core::{Payload, WorkflowId};

/// Target and content for a workflow-originated signal send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalDelivery {
    /// Workflow that should receive the signal.
    pub target_workflow_id: WorkflowId,
    /// Signal name selected by workflow code.
    pub name: String,
    /// Opaque signal payload.
    pub payload: Payload,
}
