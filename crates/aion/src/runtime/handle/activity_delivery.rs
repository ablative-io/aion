//! Synchronization between retained activity delivery and workflow death.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::EngineError;

use super::{Pid, RuntimeHandle, runtime_error};

/// One workflow's retained-outcome delivery gate.
///
/// `dead` is published while holding `barrier`, before the monitor drains the
/// workflow's retained maps. It keeps later delivery closed even while beamr's
/// exit tombstone is visible but process-table removal is still pending.
#[derive(Default)]
pub(super) struct ActivityDeliveryGate {
    barrier: Mutex<()>,
    dead: AtomicBool,
}

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
        let delivery = self.with_activity_delivery(workflow_pid, |gate| {
            self.ensure_activity_delivery_live(workflow_pid, gate)?;
            retained.insert(key, outcome);
            let marker_delivery = deliver_marker();
            if marker_delivery.is_err() {
                retained.remove(&key);
            }
            marker_delivery
        });
        if delivery.is_err() {
            self.activity_delivery_attempts.remove(&key);
        }
        delivery
    }

    /// Retain the one-based delivery attempt that produced the outcome about
    /// to be delivered for `(parent_pid, activity_sequence)` (#197).
    ///
    /// Called by the completion task's retry loop right before it retains the
    /// final payload/error, so the awaiting NIF records the terminal with the
    /// genuine attempt instead of assuming the first delivery.
    ///
    /// A typed gate-poison or dead-workflow error is logged explicitly. This
    /// best-effort metadata cannot change the completion task's `()` contract;
    /// the following outcome delivery independently returns and logs the same
    /// liveness or poison failure.
    pub(crate) fn note_delivery_attempt(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
        attempt: u32,
    ) {
        let note = self.with_activity_delivery(parent_pid, |gate| {
            self.ensure_activity_delivery_live(parent_pid, gate)?;
            self.activity_delivery_attempts
                .insert((parent_pid, activity_sequence), attempt);
            Ok(())
        });
        if let Err(error) = note {
            tracing::error!(
                %error,
                parent_pid,
                activity_sequence,
                "activity delivery attempt retention failed"
            );
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

    /// Mark a workflow dead and drop all of its retained activity state.
    ///
    /// The workflow's gate is independent from every other pid and is released
    /// immediately after the sweep. Its dead bit prevents insertion behind the
    /// sweep while beamr still exposes the tombstoned pid in its process table.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ActivityDeliveryPoisoned`] when this workflow's
    /// delivery gate was poisoned.
    pub(crate) fn drain_activity_completions(&self, workflow_pid: Pid) -> Result<(), EngineError> {
        self.with_activity_delivery(workflow_pid, |gate| {
            gate.dead.store(true, Ordering::Release);
            self.activity_results
                .retain(|(parent, _), _| *parent != workflow_pid);
            self.activity_errors
                .retain(|(parent, _), _| *parent != workflow_pid);
            self.activity_delivery_attempts
                .retain(|(parent, _), _| *parent != workflow_pid);
            Ok(())
        })?;
        self.reap_activity_delivery_gates();
        Ok(())
    }

    pub(super) fn with_activity_delivery<T, F>(
        &self,
        workflow_pid: Pid,
        operation: F,
    ) -> Result<T, EngineError>
    where
        F: FnOnce(&ActivityDeliveryGate) -> Result<T, EngineError>,
    {
        let gate = self.activity_delivery_gate(workflow_pid);
        let guard = match gate.barrier.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                drop(poisoned);
                return Err(EngineError::ActivityDeliveryPoisoned {
                    process_id: workflow_pid,
                });
            }
        };
        let result = operation(&gate);
        drop(guard);
        result
    }

    pub(super) fn ensure_activity_delivery_live(
        &self,
        workflow_pid: Pid,
        gate: &ActivityDeliveryGate,
    ) -> Result<(), EngineError> {
        if gate.dead.load(Ordering::Acquire)
            || self.scheduler.peek_exit_reason(workflow_pid).is_some()
        {
            return Err(runtime_error(format!("process {workflow_pid} is not live")));
        }
        self.ensure_live_pid(workflow_pid)
    }

    fn activity_delivery_gate(&self, workflow_pid: Pid) -> Arc<ActivityDeliveryGate> {
        self.reap_activity_delivery_gates();
        let entry = self
            .activity_delivery_gates
            .entry(workflow_pid)
            .or_default();
        Arc::clone(entry.value())
    }

    fn reap_activity_delivery_gates(&self) {
        self.activity_delivery_gates.retain(|pid, gate| {
            !gate.dead.load(Ordering::Acquire) || self.scheduler.process_table().get(*pid).is_some()
        });
    }
}
