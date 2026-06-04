//! `WorkflowHandle` process id, type, version, and status metadata.

use aion_core::WorkflowStatus;
use aion_package::ContentHash;

/// Live workflow process metadata cached in the active-execution registry.
///
/// The handle stores only the runtime process identifier value, not a runtime
/// object or scheduler state. The cached status is reconciled from the durable
/// event projection by the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowHandle {
    pid: u64,
    workflow_type: String,
    loaded_version: ContentHash,
    cached_status: WorkflowStatus,
}

impl WorkflowHandle {
    /// Creates a workflow handle from process metadata and a projected status.
    #[must_use]
    pub fn new(
        pid: u64,
        workflow_type: impl Into<String>,
        loaded_version: ContentHash,
        cached_status: WorkflowStatus,
    ) -> Self {
        Self {
            pid,
            workflow_type: workflow_type.into(),
            loaded_version,
            cached_status,
        }
    }

    /// Returns the embedded runtime process identifier value.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.pid
    }

    /// Returns the logical workflow type / entry module selected by the caller.
    #[must_use]
    pub fn workflow_type(&self) -> &str {
        &self.workflow_type
    }

    /// Returns the loaded workflow package version identifier.
    #[must_use]
    pub const fn loaded_version(&self) -> &ContentHash {
        &self.loaded_version
    }

    /// Returns the cached workflow status.
    #[must_use]
    pub const fn cached_status(&self) -> WorkflowStatus {
        self.cached_status
    }

    /// Replaces the cached status with the durable event projection result.
    pub(crate) const fn replace_projected_status(&mut self, status: WorkflowStatus) {
        self.cached_status = status;
    }
}
