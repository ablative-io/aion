//! `WorkflowHandle` process id, type, version, status, residency, and completion metadata.

use std::sync::{Arc, Mutex};

use aion_core::{Payload, RunId, WorkflowError, WorkflowId, WorkflowStatus};
use aion_package::ContentHash;
use tokio::sync::watch;

use crate::durability::Recorder;

/// Engine-internal live residency cached on an active workflow handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleResidency {
    /// The workflow has a live BEAM process.
    Resident,
    /// The workflow is durable but currently has no live process.
    Suspended,
}

/// Terminal outcome delivered to result awaiters by later terminal lifecycle transitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalOutcome {
    /// Workflow completed successfully with a result payload.
    Completed(Payload),
    /// Workflow failed terminally.
    Failed(WorkflowError),
    /// Workflow was cancelled with the durable cancellation reason.
    Cancelled(String),
    /// Workflow exceeded a timeout owned by the timer/signal cluster.
    TimedOut(String),
}

/// Multi-consumer completion notification channel for a workflow execution.
#[derive(Clone, Debug)]
pub struct CompletionNotifier {
    sender: watch::Sender<Option<TerminalOutcome>>,
}

impl CompletionNotifier {
    /// Creates an unfulfilled completion notifier.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(None);
        Self { sender }
    }

    /// Subscribes a result awaiter to the eventual terminal outcome.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<TerminalOutcome>> {
        self.sender.subscribe()
    }

    /// Publishes the terminal outcome to all current and future subscribers.
    ///
    /// Returns true when at least one receiver observed or can observe the value.
    #[must_use]
    pub fn notify(&self, outcome: TerminalOutcome) -> bool {
        self.sender.send(Some(outcome)).is_ok()
    }

    /// Returns true once a terminal outcome has been published.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.sender.borrow().is_some()
    }
}

impl Default for CompletionNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for CompletionNotifier {
    fn eq(&self, other: &Self) -> bool {
        self.sender.same_channel(&other.sender)
    }
}

impl Eq for CompletionNotifier {}

/// Constructor inputs for a live workflow handle.
pub struct WorkflowHandleParts {
    /// Logical workflow identifier assigned at start.
    pub workflow_id: WorkflowId,
    /// Concrete run identifier assigned at start.
    pub run_id: RunId,
    /// Embedded runtime process identifier.
    pub pid: u64,
    /// Logical workflow type selected by the caller.
    pub workflow_type: String,
    /// Loaded package version selected by the loader.
    pub loaded_version: ContentHash,
    /// Cached projection status initialized from the start event.
    pub cached_status: WorkflowStatus,
    /// Engine-internal residency initialized for the live process.
    pub residency: HandleResidency,
    /// Single-writer recorder created for this workflow history.
    pub recorder: Recorder,
    /// Completion notifier created for result awaiters.
    pub completion: CompletionNotifier,
}

/// Live workflow process metadata cached in the active-execution registry.
///
/// The handle stores only the runtime process identifier value, not a runtime
/// object or scheduler state. The cached status is reconciled from the durable
/// event projection by the registry. Residency is engine-internal and separate
/// from projected workflow status.
#[derive(Clone)]
pub struct WorkflowHandle {
    workflow_id: WorkflowId,
    run_id: RunId,
    pid: u64,
    workflow_type: String,
    loaded_version: ContentHash,
    cached_status: WorkflowStatus,
    residency: HandleResidency,
    recorder: Arc<Mutex<Recorder>>,
    completion: CompletionNotifier,
}

impl WorkflowHandle {
    /// Creates a workflow handle from process metadata and start-owned resources.
    #[must_use]
    pub fn new(parts: WorkflowHandleParts) -> Self {
        Self {
            workflow_id: parts.workflow_id,
            run_id: parts.run_id,
            pid: parts.pid,
            workflow_type: parts.workflow_type,
            loaded_version: parts.loaded_version,
            cached_status: parts.cached_status,
            residency: parts.residency,
            recorder: Arc::new(Mutex::new(parts.recorder)),
            completion: parts.completion,
        }
    }

    /// Returns the logical workflow identifier.
    #[must_use]
    pub const fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    /// Returns the concrete run identifier.
    #[must_use]
    pub const fn run_id(&self) -> &RunId {
        &self.run_id
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

    /// Returns the live residency tracked separately from workflow status.
    #[must_use]
    pub const fn residency(&self) -> HandleResidency {
        self.residency
    }

    /// Returns the shared single-writer recorder for later lifecycle transitions.
    #[must_use]
    pub fn recorder(&self) -> Arc<Mutex<Recorder>> {
        Arc::clone(&self.recorder)
    }

    /// Returns the completion notifier created at workflow start.
    #[must_use]
    pub const fn completion(&self) -> &CompletionNotifier {
        &self.completion
    }

    /// Replaces the cached status with the durable event projection result.
    pub(in crate::registry) const fn replace_projected_status(&mut self, status: WorkflowStatus) {
        self.cached_status = status;
    }
}

impl std::fmt::Debug for WorkflowHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowHandle")
            .field("workflow_id", &self.workflow_id)
            .field("run_id", &self.run_id)
            .field("pid", &self.pid)
            .field("workflow_type", &self.workflow_type)
            .field("loaded_version", &self.loaded_version)
            .field("cached_status", &self.cached_status)
            .field("residency", &self.residency)
            .field("completion", &self.completion)
            .finish_non_exhaustive()
    }
}

impl PartialEq for WorkflowHandle {
    fn eq(&self, other: &Self) -> bool {
        self.workflow_id == other.workflow_id
            && self.run_id == other.run_id
            && self.pid == other.pid
            && self.workflow_type == other.workflow_type
            && self.loaded_version == other.loaded_version
            && self.cached_status == other.cached_status
            && self.residency == other.residency
            && Arc::ptr_eq(&self.recorder, &other.recorder)
            && self.completion == other.completion
    }
}

impl Eq for WorkflowHandle {}
