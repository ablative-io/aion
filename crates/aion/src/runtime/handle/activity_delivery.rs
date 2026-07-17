//! Synchronization between retained activity delivery and workflow death.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use beamr::atom::Atom;
use dashmap::mapref::entry::Entry;

use crate::error::EngineError;

use super::{Pid, RuntimeHandle, runtime_error};

/// Which retained outcome map owns an activity payload.
#[derive(Clone, Copy)]
pub(super) enum ActivityOutcomeKind {
    Result,
    Error,
}

pub(super) struct RetainedActivityDelivery<V> {
    pub(super) key: (Pid, Pid),
    pub(super) outcome: V,
    pub(super) kind: ActivityOutcomeKind,
    pub(super) attempt: Option<u32>,
}

#[derive(Default)]
struct RetainedActivityParts {
    result: bool,
    error: bool,
    attempt: bool,
}

impl RetainedActivityParts {
    fn retain_outcome(&mut self, kind: ActivityOutcomeKind) {
        match kind {
            ActivityOutcomeKind::Result => self.result = true,
            ActivityOutcomeKind::Error => self.error = true,
        }
    }

    fn release_outcome(&mut self, kind: ActivityOutcomeKind) {
        match kind {
            ActivityOutcomeKind::Result => self.result = false,
            ActivityOutcomeKind::Error => self.error = false,
        }
    }

    fn is_empty(&self) -> bool {
        !self.result && !self.error && !self.attempt
    }
}

/// Mutable state serialized by one workflow's delivery gate.
#[derive(Default)]
pub(super) struct ActivityDeliveryState {
    dead: bool,
    retained: HashMap<Pid, RetainedActivityParts>,
}

impl ActivityDeliveryState {
    pub(super) fn retain_outcome(&mut self, activity_sequence: Pid, kind: ActivityOutcomeKind) {
        self.retained
            .entry(activity_sequence)
            .or_default()
            .retain_outcome(kind);
    }

    pub(super) fn retain_attempt(&mut self, activity_sequence: Pid) {
        self.retained.entry(activity_sequence).or_default().attempt = true;
    }

    fn release_outcome(&mut self, activity_sequence: Pid, kind: ActivityOutcomeKind) {
        if let Some(parts) = self.retained.get_mut(&activity_sequence) {
            parts.release_outcome(kind);
        }
        self.remove_empty(activity_sequence);
    }

    fn release_attempt(&mut self, activity_sequence: Pid) {
        if let Some(parts) = self.retained.get_mut(&activity_sequence) {
            parts.attempt = false;
        }
        self.remove_empty(activity_sequence);
    }

    fn remove_empty(&mut self, activity_sequence: Pid) {
        if self
            .retained
            .get(&activity_sequence)
            .is_some_and(RetainedActivityParts::is_empty)
        {
            self.retained.remove(&activity_sequence);
        }
    }
}

/// One workflow's retained-outcome delivery gate.
///
/// `barrier` serializes all retention with the workflow monitor's destructive
/// drain. A dead state keeps later delivery closed until beamr removes the
/// process-table entry and the monitor conditionally removes this exact gate.
#[derive(Default)]
pub(super) struct ActivityDeliveryGate {
    barrier: Mutex<ActivityDeliveryState>,
    #[cfg(test)]
    force_poisoned_acquisition: AtomicBool,
    #[cfg(test)]
    cleanup_started: AtomicBool,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct ActivityDeliveryTestSeams {
    marker_refusals: dashmap::DashMap<(Pid, Pid), u32>,
    fail_next_monitor_spawn: AtomicBool,
}

enum ActivityDeliveryLock<'a> {
    Clean(MutexGuard<'a, ActivityDeliveryState>),
    Poisoned(MutexGuard<'a, ActivityDeliveryState>),
}

impl RuntimeHandle {
    /// Retain an outcome and deliver its wake marker under the workflow gate.
    ///
    /// The outcome remains retained until the awaiting NIF takes it or the
    /// workflow monitor confirms death and drains it. A live marker refusal
    /// preserves a first publication because an executing workflow reads that
    /// keyed state on its next await; a failed duplicate publication restores
    /// the outcome and attempt metadata it replaced.
    pub(super) fn retain_activity_outcome_and_deliver_marker<V, F>(
        &self,
        workflow_pid: Pid,
        retained: &dashmap::DashMap<(Pid, Pid), V>,
        delivery: RetainedActivityDelivery<V>,
        deliver_marker: F,
    ) -> Result<(), EngineError>
    where
        F: FnOnce() -> Result<(), EngineError>,
    {
        let RetainedActivityDelivery {
            key,
            outcome,
            kind,
            attempt,
        } = delivery;
        self.with_activity_delivery(workflow_pid, |state| {
            self.ensure_activity_delivery_live(workflow_pid, state)?;
            let previous_outcome = retained.insert(key, outcome);
            state.retain_outcome(key.1, kind);
            let previous_attempt = attempt.map(|attempt| {
                let previous = self.activity_delivery_attempts.insert(key, attempt);
                state.retain_attempt(key.1);
                previous
            });
            let marker_delivery = deliver_marker();
            if marker_delivery.is_err() {
                let replaced_outcome = previous_outcome.is_some();
                if let Some(previous) = previous_outcome {
                    retained.insert(key, previous);
                }
                if let Some(previous) = previous_attempt {
                    if let Some(previous) = previous {
                        self.activity_delivery_attempts.insert(key, previous);
                    } else if replaced_outcome {
                        self.activity_delivery_attempts.remove(&key);
                        state.release_attempt(key.1);
                    }
                }
            }
            marker_delivery
        })
    }

    pub(super) fn enqueue_activity_marker(
        &self,
        workflow_pid: Pid,
        marker: Atom,
        activity_sequence: Pid,
        description: &str,
    ) -> Result<(), EngineError> {
        let attempts = self.signal_delivery.max_enqueue_attempts.max(1);
        let mut backoff = self.signal_delivery.initial_backoff;
        for attempt in 1..=attempts {
            #[cfg(test)]
            let refused_for_test =
                self.refuse_activity_marker_for_test(workflow_pid, activity_sequence);
            #[cfg(not(test))]
            let refused_for_test = false;
            if !refused_for_test && self.scheduler.enqueue_atom_message(workflow_pid, marker) {
                self.confirm_marker_wake(workflow_pid);
                return Ok(());
            }
            if self.scheduler.process_table().get(workflow_pid).is_none() {
                return Err(runtime_error(format!(
                    "failed to deliver activity marker {description} (sequence {activity_sequence}) to {workflow_pid}: process is not live"
                )));
            }
            if attempt < attempts {
                super::delivery::sleep_signal_delivery_backoff(backoff);
                backoff = super::delivery::next_signal_delivery_backoff(
                    backoff,
                    self.signal_delivery.max_backoff,
                );
            }
        }
        Err(runtime_error(format!(
            "failed to deliver activity marker {description} (sequence {activity_sequence}) to {workflow_pid} after {attempts} attempts"
        )))
    }

    #[cfg(test)]
    fn refuse_activity_marker_for_test(&self, workflow_pid: Pid, activity_sequence: Pid) -> bool {
        let key = (workflow_pid, activity_sequence);
        let Some(mut remaining) = self
            .activity_delivery_test_seams
            .marker_refusals
            .get_mut(&key)
        else {
            return false;
        };
        *remaining = remaining.saturating_sub(1);
        let exhausted = *remaining == 0;
        drop(remaining);
        if exhausted {
            self.activity_delivery_test_seams
                .marker_refusals
                .remove(&key);
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn force_activity_marker_refusals_for_test(
        &self,
        workflow_pid: Pid,
        activity_sequence: Pid,
        refusals: u32,
    ) {
        self.activity_delivery_test_seams
            .marker_refusals
            .insert((workflow_pid, activity_sequence), refusals.max(1));
    }

    #[cfg(test)]
    pub(crate) fn force_next_monitor_spawn_failure_for_test(&self) {
        self.activity_delivery_test_seams
            .fail_next_monitor_spawn
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn take_monitor_spawn_failure_for_test(&self) -> bool {
        self.activity_delivery_test_seams
            .fail_next_monitor_spawn
            .swap(false, Ordering::AcqRel)
    }

    /// Atomically take a retained outcome and its one-based delivery attempt.
    ///
    /// A missing attempt means the outcome came from a path that never retries
    /// (outbox re-delivery or an in-VM child) and is the first delivery.
    pub(super) fn take_activity_outcome<V>(
        &self,
        workflow_pid: Pid,
        activity_sequence: Pid,
        retained: &dashmap::DashMap<(Pid, Pid), V>,
        kind: ActivityOutcomeKind,
    ) -> Result<Option<(V, Option<u32>)>, EngineError> {
        let Some(gate) = self
            .activity_delivery_gates
            .get(&workflow_pid)
            .map(|entry| Arc::clone(entry.value()))
        else {
            return Ok(None);
        };
        let mut state = match Self::lock_activity_delivery(&gate) {
            ActivityDeliveryLock::Clean(state) => state,
            ActivityDeliveryLock::Poisoned(state) => {
                drop(state);
                return Err(EngineError::ActivityDeliveryPoisoned {
                    process_id: workflow_pid,
                });
            }
        };
        let outcome = retained
            .remove(&(workflow_pid, activity_sequence))
            .map(|(_, outcome)| outcome);
        let Some(outcome) = outcome else {
            return Ok(None);
        };
        state.release_outcome(activity_sequence, kind);
        let attempt = self
            .activity_delivery_attempts
            .remove(&(workflow_pid, activity_sequence))
            .map(|(_, attempt)| attempt);
        if attempt.is_some() {
            state.release_attempt(activity_sequence);
        }
        Ok(Some((outcome, attempt)))
    }

    /// Mark a workflow dead and drop all of its retained activity state.
    ///
    /// Cleanup acquires a poisoned barrier solely for destructive draining. It
    /// never resumes ordinary delivery, and still returns the typed poison error
    /// after removing every payload and attempt indexed by this workflow.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ActivityDeliveryPoisoned`] after completing the
    /// destructive drain when this workflow's delivery gate was poisoned.
    pub(crate) fn drain_activity_completions(&self, workflow_pid: Pid) -> Result<(), EngineError> {
        let gate = self.activity_delivery_gate_for_cleanup(workflow_pid);
        #[cfg(test)]
        gate.cleanup_started.store(true, Ordering::Release);

        let (mut state, poisoned) = match Self::lock_activity_delivery(&gate) {
            ActivityDeliveryLock::Clean(state) => (state, false),
            ActivityDeliveryLock::Poisoned(state) => (state, true),
        };
        state.dead = true;
        let retained = std::mem::take(&mut state.retained);
        for activity_sequence in retained.keys() {
            let key = (workflow_pid, *activity_sequence);
            self.activity_results.remove(&key);
            self.activity_errors.remove(&key);
            self.activity_delivery_attempts.remove(&key);
        }
        drop(state);

        if self.scheduler.process_table().get(workflow_pid).is_none() {
            self.remove_activity_delivery_gate(workflow_pid, &gate);
        }
        if poisoned {
            Err(EngineError::ActivityDeliveryPoisoned {
                process_id: workflow_pid,
            })
        } else {
            Ok(())
        }
    }

    /// Wait for beamr's process-table removal, then reap this workflow's exact gate.
    ///
    /// The production monitor thread is already dedicated to this `Pid`, so this
    /// targeted wait cannot couple another workflow's delivery to the dead pid.
    pub(crate) fn finish_activity_delivery_cleanup(&self, workflow_pid: Pid) {
        let Some(gate) = self
            .activity_delivery_gates
            .get(&workflow_pid)
            .map(|entry| Arc::clone(entry.value()))
        else {
            return;
        };
        while self.scheduler.process_table().get(workflow_pid).is_some() {
            std::thread::sleep(self.signal_delivery.initial_backoff);
        }
        self.remove_activity_delivery_gate(workflow_pid, &gate);
    }

    pub(super) fn with_activity_delivery<T, F>(
        &self,
        workflow_pid: Pid,
        operation: F,
    ) -> Result<T, EngineError>
    where
        F: FnOnce(&mut ActivityDeliveryState) -> Result<T, EngineError>,
    {
        let (gate, created) = self.activity_delivery_gate(workflow_pid)?;
        let mut state = match Self::lock_activity_delivery(&gate) {
            ActivityDeliveryLock::Clean(state) => state,
            ActivityDeliveryLock::Poisoned(state) => {
                drop(state);
                return Err(EngineError::ActivityDeliveryPoisoned {
                    process_id: workflow_pid,
                });
            }
        };
        let result = operation(&mut state);
        if result.is_err() && self.scheduler.peek_exit_reason(workflow_pid).is_some() {
            state.dead = true;
        }
        let remove_failed_creation = created && state.dead && state.retained.is_empty();
        drop(state);
        if remove_failed_creation {
            self.remove_activity_delivery_gate(workflow_pid, &gate);
        }
        result
    }

    pub(super) fn ensure_activity_delivery_live(
        &self,
        workflow_pid: Pid,
        state: &mut ActivityDeliveryState,
    ) -> Result<(), EngineError> {
        if state.dead || self.scheduler.peek_exit_reason(workflow_pid).is_some() {
            state.dead = true;
            return Err(runtime_error(format!("process {workflow_pid} is not live")));
        }
        Ok(())
    }

    fn activity_delivery_gate(
        &self,
        workflow_pid: Pid,
    ) -> Result<(Arc<ActivityDeliveryGate>, bool), EngineError> {
        match self.activity_delivery_gates.entry(workflow_pid) {
            Entry::Occupied(entry) => Ok((Arc::clone(entry.get()), false)),
            Entry::Vacant(entry) => {
                if self.scheduler.peek_exit_reason(workflow_pid).is_some() {
                    return Err(runtime_error(format!("process {workflow_pid} is not live")));
                }
                self.ensure_live_pid(workflow_pid)?;
                let gate = Arc::new(ActivityDeliveryGate::default());
                entry.insert(Arc::clone(&gate));
                Ok((gate, true))
            }
        }
    }

    fn activity_delivery_gate_for_cleanup(&self, workflow_pid: Pid) -> Arc<ActivityDeliveryGate> {
        match self.activity_delivery_gates.entry(workflow_pid) {
            Entry::Occupied(entry) => Arc::clone(entry.get()),
            Entry::Vacant(entry) => {
                let gate = Arc::new(ActivityDeliveryGate::default());
                entry.insert(Arc::clone(&gate));
                gate
            }
        }
    }

    fn remove_activity_delivery_gate(
        &self,
        workflow_pid: Pid,
        expected: &Arc<ActivityDeliveryGate>,
    ) {
        self.activity_delivery_gates
            .remove_if(&workflow_pid, |_, current| Arc::ptr_eq(current, expected));
    }

    fn lock_activity_delivery(gate: &ActivityDeliveryGate) -> ActivityDeliveryLock<'_> {
        match gate.barrier.lock() {
            Ok(state) => {
                #[cfg(test)]
                if gate.force_poisoned_acquisition.load(Ordering::Acquire) {
                    return ActivityDeliveryLock::Poisoned(state);
                }
                ActivityDeliveryLock::Clean(state)
            }
            Err(poisoned) => ActivityDeliveryLock::Poisoned(poisoned.into_inner()),
        }
    }

    #[cfg(test)]
    pub(crate) fn force_activity_delivery_poison_for_test(
        &self,
        workflow_pid: Pid,
    ) -> Result<(), EngineError> {
        let (gate, _) = self.activity_delivery_gate(workflow_pid)?;
        gate.force_poisoned_acquisition
            .store(true, Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn activity_delivery_index_contains_for_test(
        &self,
        workflow_pid: Pid,
        activity_sequence: Pid,
    ) -> Result<bool, EngineError> {
        let Some(gate) = self
            .activity_delivery_gates
            .get(&workflow_pid)
            .map(|entry| Arc::clone(entry.value()))
        else {
            return Ok(false);
        };
        match Self::lock_activity_delivery(&gate) {
            ActivityDeliveryLock::Clean(state) => {
                Ok(state.retained.contains_key(&activity_sequence))
            }
            ActivityDeliveryLock::Poisoned(state) => {
                drop(state);
                Err(EngineError::ActivityDeliveryPoisoned {
                    process_id: workflow_pid,
                })
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn activity_delivery_gate_count(&self) -> usize {
        self.activity_delivery_gates.len()
    }

    #[cfg(test)]
    pub(crate) fn retained_activity_attempt_count_for_test(&self) -> usize {
        self.activity_delivery_attempts.len()
    }

    #[cfg(test)]
    pub(super) fn activity_delivery_cleanup_started_for_test(&self, workflow_pid: Pid) -> bool {
        self.activity_delivery_gates
            .get(&workflow_pid)
            .is_some_and(|gate| gate.cleanup_started.load(Ordering::Acquire))
    }
}
