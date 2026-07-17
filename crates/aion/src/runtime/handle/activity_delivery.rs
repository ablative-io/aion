//! Synchronization between retained activity delivery and workflow death.

use crate::error::EngineError;

use super::{Pid, RuntimeHandle};

impl RuntimeHandle {
    pub(super) fn retain_activity_outcome_until_marker_delivery<V, F>(
        &self,
        workflow_pid: Pid,
        retained: &dashmap::DashMap<(Pid, Pid), V>,
        key: (Pid, Pid),
        outcome: V,
        deliver_marker: F,
    ) -> Result<(), EngineError>
    where
        F: FnOnce() -> Result<(), EngineError>,
    {
        let _delivery_guard = self.lock_activity_delivery_for(workflow_pid);
        self.ensure_live_pid(workflow_pid)?;
        retained.insert(key, outcome);
        let delivery = deliver_marker();
        if delivery.is_err() {
            retained.remove(&key);
        }
        delivery
    }

    /// Retain the one-based delivery attempt that produced the outcome about
    /// to be delivered for `(parent_pid, activity_sequence)` (#197).
    ///
    /// Called by the completion task's retry loop right before it retains the
    /// final payload/error, so the awaiting NIF records the terminal with the
    /// genuine attempt instead of assuming the first delivery.
    pub(crate) fn note_delivery_attempt(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
        attempt: u32,
    ) {
        let _delivery_guard = self.lock_activity_delivery_for(parent_pid);
        if self.ensure_live_pid(parent_pid).is_ok() {
            self.activity_delivery_attempts
                .insert((parent_pid, activity_sequence), attempt);
        }
    }

    /// Take the noted delivery attempt for a retained outcome, if any.
    ///
    /// `None` means the outcome arrived through a path that never retries
    /// (outbox re-delivery, in-VM children) and is the first delivery.
    pub(crate) fn take_delivery_attempt(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
    ) -> Option<u32> {
        self.activity_delivery_attempts
            .remove(&(parent_pid, activity_sequence))
            .map(|(_, attempt)| attempt)
    }

    /// Drop every retained activity completion and failure for a workflow pid.
    ///
    /// Called from the workflow process monitor when the process exits: a
    /// completion delivered after the workflow stopped awaiting it is never
    /// taken by an await and would otherwise be retained forever (D5).
    pub(crate) fn drain_activity_completions(&self, workflow_pid: Pid) {
        let _delivery_guard = self.lock_activity_delivery();
        self.activity_results
            .retain(|(parent, _), _| *parent != workflow_pid);
        self.activity_errors
            .retain(|(parent, _), _| *parent != workflow_pid);
        self.activity_delivery_attempts
            .retain(|(parent, _), _| *parent != workflow_pid);
        #[cfg(test)]
        self.activity_drain_observed
            .store(workflow_pid, std::sync::atomic::Ordering::Release);
        // beamr publishes its exit tombstone before removing the live-table
        // and enqueueable-body entries. Keep the barrier until the live-table
        // removal is visible so delivery cannot insert behind this drain.
        while self.scheduler.process_table().get(workflow_pid).is_some() {
            std::thread::yield_now();
        }
    }

    pub(super) fn lock_activity_delivery(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.activity_delivery_barrier.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(super) fn lock_activity_delivery_for(
        &self,
        workflow_pid: Pid,
    ) -> std::sync::MutexGuard<'_, ()> {
        let _ = workflow_pid;
        #[cfg(test)]
        self.activity_delivery_waiting
            .store(workflow_pid, std::sync::atomic::Ordering::Release);
        self.lock_activity_delivery()
    }
}
