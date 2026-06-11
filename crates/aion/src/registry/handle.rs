//! `WorkflowHandle` process id, type, version, status, residency, and completion metadata.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use aion_core::{Payload, RunId, WorkflowError, WorkflowId, WorkflowStatus};
use aion_package::ContentHash;
use tokio::sync::{Mutex, watch};

use crate::durability::Recorder;

/// Engine-internal live residency cached on an active workflow handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residency {
    /// The workflow has a live BEAM process.
    Resident,
    /// The workflow is durable but currently has no live process.
    Suspended,
}

/// Backward-compatible alias for the engine-internal residency type.
pub type HandleResidency = Residency;

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
    /// Workflow continued as a new run under the same workflow identifier.
    ContinuedAsNew {
        /// Opaque workflow input payload carried into the new run.
        input: Payload,
        /// Workflow type override for the new run, when present.
        workflow_type: Option<String>,
        /// Run identifier for the current run that continued.
        parent_run_id: RunId,
    },
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
    /// The value is stored even when no result waiter is currently subscribed, so
    /// a waiter that subscribed from a still-held handle before deregistration can
    /// observe the terminal outcome instead of hanging on an unfulfilled channel.
    pub fn notify(&self, outcome: TerminalOutcome) {
        drop(self.sender.send_replace(Some(outcome)));
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
    pub residency: Residency,
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
    residency: Residency,
    recorder: Arc<Mutex<Recorder>>,
    completion: CompletionNotifier,
    deterministic_nif_sequence: Arc<AtomicU64>,
    activity_ordinal_sequence: Arc<AtomicU64>,
    timer_ordinal_sequence: Arc<AtomicU64>,
    child_ordinal_sequence: Arc<AtomicU64>,
    signal_receive_counts: Arc<dashmap::DashMap<String, u64>>,
    signal_send_counts: Arc<dashmap::DashMap<String, u64>>,
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
            deterministic_nif_sequence: Arc::new(AtomicU64::new(0)),
            activity_ordinal_sequence: Arc::new(AtomicU64::new(0)),
            timer_ordinal_sequence: Arc::new(AtomicU64::new(0)),
            child_ordinal_sequence: Arc::new(AtomicU64::new(0)),
            signal_receive_counts: Arc::new(dashmap::DashMap::new()),
            signal_send_counts: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Allocate `count` consecutive activity correlation ordinals.
    ///
    /// The sequence is monotonic per run and shared by every NIF call the
    /// run makes (handles clone the same counter), so distinct workflow
    /// steps never collide on correlation keys. A re-spawned run (crash
    /// recovery, continue-as-new) gets a fresh handle and counter, and its
    /// replayed code re-allocates the same ordinals deterministically.
    #[must_use]
    pub fn allocate_activity_ordinals(&self, count: u64) -> u64 {
        self.activity_ordinal_sequence
            .fetch_add(count, std::sync::atomic::Ordering::SeqCst)
    }

    /// Allocate `count` consecutive child-workflow spawn ordinals.
    ///
    /// Same determinism contract as [`Self::allocate_activity_ordinals`]:
    /// monotonic per run, shared by every NIF call the run makes, and
    /// re-allocated identically by replayed code on a re-spawned run. The
    /// n-th allocated ordinal correlates the n-th `spawn_child` call with
    /// the n-th recorded `ChildWorkflowStarted` in the run's history
    /// segment, independent of event sequence numbers and of any
    /// asynchronous-arrival events interleaved between spawns.
    #[must_use]
    pub fn allocate_child_ordinals(&self, count: u64) -> u64 {
        self.child_ordinal_sequence
            .fetch_add(count, std::sync::atomic::Ordering::SeqCst)
    }

    /// Allocate `count` consecutive timer ordinals.
    ///
    /// Same determinism contract as [`Self::allocate_activity_ordinals`]:
    /// monotonic per run, shared by every NIF call the run makes, and
    /// re-allocated identically by replayed code on a re-spawned run. Used
    /// to derive anonymous timer identities (`sleep`, `with_timeout` scope
    /// deadlines) that stay stable across crash-recovery replay.
    #[must_use]
    pub fn allocate_timer_ordinals(&self, count: u64) -> u64 {
        self.timer_ordinal_sequence
            .fetch_add(count, std::sync::atomic::Ordering::SeqCst)
    }

    /// Activity ordinals allocated so far by this run's execution.
    ///
    /// Read-only progress probe: replay re-allocates deterministically, so a
    /// value below the run segment's recorded `ActivityScheduled` count means
    /// the run is still mid-replay.
    #[must_use]
    pub fn activity_ordinals_allocated(&self) -> u64 {
        self.activity_ordinal_sequence
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Timer ordinals allocated so far by this run's execution.
    ///
    /// Same replay-progress contract as [`Self::activity_ordinals_allocated`],
    /// measured against recorded anonymous `TimerStarted` events.
    #[must_use]
    pub fn timer_ordinals_allocated(&self) -> u64 {
        self.timer_ordinal_sequence
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Child-workflow ordinals allocated so far by this run's execution.
    ///
    /// Same replay-progress contract as [`Self::activity_ordinals_allocated`],
    /// measured against recorded `ChildWorkflowStarted` events.
    #[must_use]
    pub fn child_ordinals_allocated(&self) -> u64 {
        self.child_ordinal_sequence
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Number of `receive_signal(name)` calls this run has completed.
    ///
    /// Drives the run-scoped consumption index for signal awaits: the k-th
    /// completed receive for a name consumes the k-th recorded
    /// `SignalReceived` for that name in this run's segment. Replayed code
    /// re-executes the same receives in order and re-derives the same
    /// indices; a timed-out receive consumes nothing and does not advance.
    #[must_use]
    pub fn signal_receives_consumed(&self, name: &str) -> u64 {
        self.signal_receive_counts
            .get(name)
            .map_or(0, |entry| *entry)
    }

    /// Advance the completed-receive count for `name` by one.
    pub fn mark_signal_receive_consumed(&self, name: &str) {
        *self
            .signal_receive_counts
            .entry(name.to_owned())
            .or_insert(0) += 1;
    }

    /// Number of `send_signal(name)` calls this run has completed.
    ///
    /// Drives the run-scoped correlation index for sends: the k-th completed
    /// send for a name correlates with the k-th recorded `SignalSent` for
    /// that name in this run's segment. Replayed code re-executes the same
    /// sends in order and re-derives the same indices, independent of any
    /// same-name arrivals recorded around them.
    #[must_use]
    pub fn signal_sends_completed(&self, name: &str) -> u64 {
        self.signal_send_counts.get(name).map_or(0, |entry| *entry)
    }

    /// Advance the completed-send count for `name` by one.
    pub fn mark_signal_send_completed(&self, name: &str) {
        *self.signal_send_counts.entry(name.to_owned()).or_insert(0) += 1;
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
    pub const fn residency(&self) -> Residency {
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

    /// Returns and advances the workflow-local deterministic NIF call sequence.
    #[must_use]
    pub fn next_deterministic_nif_sequence(&self) -> u64 {
        self.deterministic_nif_sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Replaces the cached status with the durable event projection result.
    pub(in crate::registry) const fn replace_projected_status(&mut self, status: WorkflowStatus) {
        self.cached_status = status;
    }

    /// Replaces the engine-internal residency without changing projected status.
    pub(in crate::registry) const fn replace_residency(&mut self, residency: Residency) {
        self.residency = residency;
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
            && Arc::ptr_eq(
                &self.deterministic_nif_sequence,
                &other.deterministic_nif_sequence,
            )
    }
}

impl Eq for WorkflowHandle {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CompletionNotifier, TerminalOutcome};

    fn payload(label: &str) -> Result<aion_core::Payload, aion_core::PayloadError> {
        aion_core::Payload::from_json(&json!({ "label": label }))
    }

    #[test]
    fn completion_notifier_stores_outcome_without_active_receiver()
    -> Result<(), aion_core::PayloadError> {
        let notifier = CompletionNotifier::new();
        let receiver = notifier.subscribe();
        drop(receiver);
        let result = payload("completed")?;

        notifier.notify(TerminalOutcome::Completed(result.clone()));
        let late_receiver = notifier.subscribe();

        assert_eq!(
            late_receiver.borrow().clone(),
            Some(TerminalOutcome::Completed(result))
        );
        assert!(notifier.is_completed());
        Ok(())
    }
}
