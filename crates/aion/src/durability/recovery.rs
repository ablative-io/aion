//! Recovery: enumerate active workflows and replay-and-resume on startup.

use aion_core::{Event, RunId, WorkflowId};
use aion_package::ContentHash;

use crate::{EngineError, LoadedWorkflows, Pid};

/// Process metadata reconstructed by AD while the engine builder enumerates
/// active workflow histories.
///
/// AE-011 deliberately does not implement replay. The builder reads active
/// histories, extracts the durable workflow type, then delegates to this seam to
/// recover the concrete run identifier, package version, and runtime process id
/// that should be registered as live.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveWorkflowRecovery {
    /// Concrete run being recovered for the logical workflow id.
    pub run_id: RunId,
    /// Package content hash/version that this run started on.
    pub loaded_version: ContentHash,
    /// Runtime process id recovered or spawned by AD replay.
    pub pid: Pid,
}

/// AD recovery/replay hook invoked by [`crate::EngineBuilder::build`].
pub trait ActiveWorkflowRecoverySeam: Send + Sync {
    /// Recover one active workflow's runtime metadata from durable history.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when replay metadata is unavailable or AD cannot
    /// recover the workflow process.
    fn recover_active_workflow(
        &self,
        workflow_id: &WorkflowId,
        workflow_type: &str,
        history: &[Event],
        loaded_workflows: &LoadedWorkflows,
    ) -> Result<ActiveWorkflowRecovery, EngineError>;
}

/// Placeholder AD seam for this cluster.
///
/// Later AD work replaces this object with replay that derives the run id,
/// started package version, and recovered workflow process. Returning a typed
/// load error is intentional: AE-011 must not invent a run id or pick the latest
/// loaded package version for active durable workflows.
#[derive(Debug, Default)]
pub struct DeferredActiveWorkflowRecovery;

impl ActiveWorkflowRecoverySeam for DeferredActiveWorkflowRecovery {
    fn recover_active_workflow(
        &self,
        workflow_id: &WorkflowId,
        workflow_type: &str,
        history: &[Event],
        loaded_workflows: &LoadedWorkflows,
    ) -> Result<ActiveWorkflowRecovery, EngineError> {
        let _ = (history, loaded_workflows);
        Err(EngineError::Load {
            reason: format!(
                "active workflow `{workflow_id}` of type `{workflow_type}` requires AD replay metadata before builder recovery can register it"
            ),
        })
    }
}
