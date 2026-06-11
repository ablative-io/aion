//! Mailbox delivery surface of [`RuntimeHandle`]: wake markers, two-phase
//! activity completion retention, and the retry-tolerant enqueue path.
//!
//! Markers are pure wakes — durable state lives in recorded history or the
//! retained completion maps, never in the marker itself.

use aion_core::{ActivityError, ActivityErrorKind, ContentType, Payload};
use beamr::atom::Atom;
use beamr::process::ExitReason;

use crate::error::EngineError;

use super::{Pid, RuntimeHandle, runtime_error};
use crate::runtime::payload::term_to_payload;

impl RuntimeHandle {
    /// Block until an activity exits, then surface its success or failure to the parent.
    ///
    /// Normal returns become typed payload results queued for the workflow and
    /// abnormal exits become typed activity errors that can be read alongside the
    /// trapped EXIT message delivered by the runtime link.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the parent is not live, the result
    /// term cannot be converted to a payload, or mailbox delivery fails.
    pub fn propagate_activity_outcome(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(parent_pid)?;
        let (reason, owned_result) = self.scheduler.run_until_exit(activity_pid);
        self.release_spawn_heaps(activity_pid);
        if reason == ExitReason::Normal {
            let payload = term_to_payload(owned_result.root(), &self.atom_table)?;
            self.deliver_activity_result(parent_pid, activity_pid, payload)
        } else {
            let error = self
                .activity_errors
                .get(&(parent_pid, activity_pid))
                .map_or_else(
                    || ActivityError {
                        kind: ActivityErrorKind::Terminal,
                        message: format!("activity process {activity_pid} exited: {reason:?}"),
                        details: None,
                    },
                    |entry| entry.clone(),
                );
            self.deliver_activity_error(parent_pid, activity_pid, error)
        }
    }

    /// Deliver a recorded signal wake marker to the workflow mailbox surface.
    ///
    /// The marker is a pure wake: the signal payload was already durably
    /// recorded by the signal router before delivery, and the awaiting NIF
    /// resolves it from recorded history. Nothing is retained here.
    ///
    /// Blocking variant for synchronous callers (engine-seam trait impls and
    /// scheduler-thread paths); async tasks use
    /// [`Self::deliver_signal_received_async`] so their executor threads are
    /// never parked in `std::thread::sleep`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// mailbox marker cannot be queued.
    pub fn deliver_signal_received(&self, workflow_pid: Pid) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        self.wait_for_process_ready(workflow_pid)?;
        let marker = self.atom_table.intern("aion_signal_received");
        self.enqueue_signal_marker_with_retry(workflow_pid, marker)
    }

    /// Async variant of [`Self::deliver_signal_received`] for runtime tasks:
    /// the readiness wait and the enqueue retry yield to the executor
    /// instead of blocking its worker thread.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// mailbox marker cannot be queued.
    pub(crate) async fn deliver_signal_received_async(
        &self,
        workflow_pid: Pid,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        self.wait_for_process_ready_async(workflow_pid).await?;
        let marker = self.atom_table.intern("aion_signal_received");
        self.enqueue_signal_marker_with_retry_async(workflow_pid, marker)
            .await
    }

    /// Deliver a pending-query wake marker to the workflow mailbox surface.
    ///
    /// The marker is a pure wake: the pending query (id and name) was already
    /// queued in the engine NIF state by the query mailbox engine, and the
    /// woken suspending await drains it through the query-pump entry check.
    /// Nothing is retained here and nothing is recorded.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// mailbox marker cannot be queued.
    pub(crate) fn deliver_query_request(&self, workflow_pid: Pid) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        self.wait_for_process_ready(workflow_pid)?;
        let marker = self.atom_table.intern("aion_query");
        self.enqueue_signal_marker_with_retry(workflow_pid, marker)
    }

    /// Deliver a recorded child-terminal wake marker to the parent workflow
    /// mailbox surface.
    ///
    /// The marker is a pure wake: the child's terminal outcome was already
    /// durably recorded into the parent's history (as
    /// `ChildWorkflowCompleted`/`ChildWorkflowFailed`) by the child-terminal
    /// watcher before delivery, and the awaiting NIF resolves it from
    /// recorded history. Nothing is retained here.
    ///
    /// Async by contract: the only caller is the child-terminal watcher on
    /// the single-worker child-task runtime, where a blocking readiness wait
    /// would serialize every other watcher's delivery behind it (worst case
    /// N × `ready_timeout` under fan-out).
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// mailbox marker cannot be queued.
    pub(crate) async fn deliver_child_terminal(
        &self,
        workflow_pid: Pid,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        self.wait_for_process_ready_async(workflow_pid).await?;
        let marker = self.atom_table.intern("aion_child_terminal");
        self.enqueue_signal_marker_with_retry_async(workflow_pid, marker)
            .await
    }

    /// Deliver a two-phase activity completion marker to the workflow mailbox.
    ///
    /// The structured `{activity_complete, CorrelationId, Result}` payload is
    /// retained in the runtime boundary, and an atom marker wakes any suspended
    /// selective receive. The await NIF resolves the retained payload by
    /// correlation id after consuming the marker.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// marker cannot be queued.
    pub(crate) fn deliver_activity_completion_message(
        &self,
        workflow_pid: Pid,
        correlation_id: &str,
        result: String,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        let activity_id = correlation_to_activity_pid(correlation_id)?;
        self.activity_results.insert(
            (workflow_pid, activity_id),
            Payload::new(ContentType::Json, result.into_bytes()),
        );
        let marker = self.atom_table.intern("activity_complete");
        self.enqueue_activity_marker(workflow_pid, marker, correlation_id)
    }

    /// Deliver a two-phase activity failure marker to the workflow mailbox.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// marker cannot be queued.
    pub(crate) fn deliver_activity_failure_message(
        &self,
        workflow_pid: Pid,
        correlation_id: &str,
        reason: String,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        let activity_id = correlation_to_activity_pid(correlation_id)?;
        self.activity_errors
            .insert((workflow_pid, activity_id), activity_failure(reason));
        let marker = self.atom_table.intern("activity_failed");
        self.enqueue_activity_marker(workflow_pid, marker, correlation_id)
    }

    /// Deliver a successful activity result payload to the workflow mailbox surface.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// mailbox marker cannot be queued.
    pub fn deliver_activity_result(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
        payload: Payload,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(parent_pid)?;
        self.activity_results
            .insert((parent_pid, activity_pid), payload);
        let marker = self.atom_table.intern("aion_activity_result");
        if self.scheduler.enqueue_atom_message(parent_pid, marker) {
            self.confirm_marker_wake(parent_pid);
            Ok(())
        } else {
            Err(runtime_error(format!(
                "failed to deliver activity result from {activity_pid} to {parent_pid}"
            )))
        }
    }

    /// Wake a suspended workflow process so blocking awaits re-run their
    /// two-phase resolution (a fired timer, an expired `with_timeout`
    /// deadline, or any other recorded arrival).
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow process is not
    /// live or the wake marker cannot be queued.
    pub(crate) fn wake_workflow(&self, workflow_pid: Pid) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        let marker = self.atom_table.intern("aion_timer_fired");
        // Retry covers the transient just-spawned/executing windows where
        // beamr's enqueue declines; a recovery-re-armed timer can fire
        // before the recovered process slot is fully materialized.
        self.enqueue_signal_marker_with_retry(workflow_pid, marker)
    }

    fn enqueue_activity_marker(
        &self,
        workflow_pid: Pid,
        marker: Atom,
        correlation_id: &str,
    ) -> Result<(), EngineError> {
        if self.scheduler.enqueue_atom_message(workflow_pid, marker) {
            self.confirm_marker_wake(workflow_pid);
            tracing::debug!(
                workflow_pid,
                correlation_id,
                "delivered activity completion marker to workflow mailbox via scheduler queue"
            );
            Ok(())
        } else {
            Err(runtime_error(format!(
                "failed to deliver activity completion marker {correlation_id} to {workflow_pid}"
            )))
        }
    }

    /// Store a typed activity error for a trapped activity EXIT signal.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow process is not live.
    pub fn deliver_activity_error(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
        error: ActivityError,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(parent_pid)?;
        self.activity_errors
            .insert((parent_pid, activity_pid), error);
        Ok(())
    }

    /// Read a previously delivered activity result payload.
    #[must_use]
    pub fn activity_result(&self, parent_pid: Pid, activity_pid: Pid) -> Option<Payload> {
        self.activity_results
            .get(&(parent_pid, activity_pid))
            .map(|entry| entry.clone())
    }

    /// Read a previously delivered activity error associated with a trapped exit.
    #[must_use]
    pub fn activity_error(&self, parent_pid: Pid, activity_pid: Pid) -> Option<ActivityError> {
        self.activity_errors
            .get(&(parent_pid, activity_pid))
            .map(|entry| entry.clone())
    }

    pub(crate) fn take_activity_result(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
    ) -> Option<Payload> {
        self.activity_results
            .remove(&(parent_pid, activity_sequence))
            .map(|(_, payload)| payload)
    }

    pub(crate) fn take_activity_error(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
    ) -> Option<ActivityError> {
        self.activity_errors
            .remove(&(parent_pid, activity_sequence))
            .map(|(_, error)| error)
    }

    /// Drop every retained activity completion and failure for a workflow pid.
    ///
    /// Called from the workflow process monitor when the process exits: a
    /// completion delivered after the workflow stopped awaiting it — a race
    /// loser's late settle, or any delivery after exit — is never `take`n by
    /// an await and would otherwise be retained forever (D5).
    pub(crate) fn drain_activity_completions(&self, workflow_pid: Pid) {
        self.activity_results
            .retain(|(parent, _), _| *parent != workflow_pid);
        self.activity_errors
            .retain(|(parent, _), _| *parent != workflow_pid);
    }

    /// Number of retained two-phase activity completion entries (results
    /// plus failures) across every workflow process.
    ///
    /// Diagnostic surface: after a workflow exits, the monitor drain must
    /// leave nothing behind for its pid, so an engine with no live awaits
    /// should report zero.
    #[must_use]
    pub fn retained_activity_completions(&self) -> usize {
        self.activity_results.len() + self.activity_errors.len()
    }

    pub(crate) fn activity_complete_atom(&self) -> Atom {
        self.atom_table.intern("activity_complete")
    }

    pub(crate) fn activity_failed_atom(&self) -> Atom {
        self.atom_table.intern("activity_failed")
    }

    pub(crate) fn activity_result_atom(&self) -> Atom {
        self.atom_table.intern("aion_activity_result")
    }

    pub(crate) fn signal_received_atom(&self) -> Atom {
        self.atom_table.intern("aion_signal_received")
    }

    pub(crate) fn timer_fired_atom(&self) -> Atom {
        self.atom_table.intern("aion_timer_fired")
    }

    pub(crate) fn query_marker_atom(&self) -> Atom {
        self.atom_table.intern("aion_query")
    }

    pub(crate) fn child_terminal_atom(&self) -> Atom {
        self.atom_table.intern("aion_child_terminal")
    }

    pub(crate) fn wait_for_process_ready(&self, pid: Pid) -> Result<(), EngineError> {
        let deadline = std::time::Instant::now() + self.signal_delivery.ready_timeout;
        while std::time::Instant::now() < deadline {
            if self.scheduler.trap_exit(pid).is_some() {
                return Ok(());
            }
            sleep_signal_delivery_backoff(self.signal_delivery.initial_backoff);
        }
        self.scheduler
            .trap_exit(pid)
            .map(|_| ())
            .ok_or_else(|| runtime_error(format!("process {pid} is not ready")))
    }

    /// Async twin of [`Self::wait_for_process_ready`]: identical readiness
    /// semantics, but the waits yield to the executor (`tokio::time::sleep`)
    /// so one slow-to-materialize process never parks a worker thread other
    /// deliveries share.
    pub(crate) async fn wait_for_process_ready_async(&self, pid: Pid) -> Result<(), EngineError> {
        let deadline = std::time::Instant::now() + self.signal_delivery.ready_timeout;
        while std::time::Instant::now() < deadline {
            if self.scheduler.trap_exit(pid).is_some() {
                return Ok(());
            }
            yield_signal_delivery_backoff(self.signal_delivery.initial_backoff).await;
        }
        self.scheduler
            .trap_exit(pid)
            .map(|_| ())
            .ok_or_else(|| runtime_error(format!("process {pid} is not ready")))
    }

    fn enqueue_signal_marker_with_retry(
        &self,
        workflow_pid: Pid,
        marker: Atom,
    ) -> Result<(), EngineError> {
        let attempts = self.signal_delivery.max_enqueue_attempts.max(1);
        let mut backoff = self.signal_delivery.initial_backoff;
        for attempt in 1..=attempts {
            if self.scheduler.enqueue_atom_message(workflow_pid, marker) {
                self.confirm_marker_wake(workflow_pid);
                return Ok(());
            }

            if self.scheduler.process_table().get(workflow_pid).is_none() {
                return Err(runtime_error(format!(
                    "failed to deliver signal to workflow process {workflow_pid}: process is not live"
                )));
            }

            if attempt < attempts {
                // beamr 0.3.15 normal spawn publishes the PID before a scheduler
                // worker materializes the process body from its SpawnRequest. It
                // also exposes an Executing slot while the process is running.
                // enqueue_atom_message only accepts a Present slot, so an alive
                // just-spawned or currently executing process can transiently
                // return false even after the liveness/ready gate above.
                sleep_signal_delivery_backoff(backoff);
                backoff = next_signal_delivery_backoff(backoff, self.signal_delivery.max_backoff);
            }
        }

        Err(runtime_error(format!(
            "failed to deliver signal to workflow process {workflow_pid} after {attempts} attempts"
        )))
    }

    /// Async twin of [`Self::enqueue_signal_marker_with_retry`]: identical
    /// retry policy over the same just-spawned/executing windows, with the
    /// backoff yielded to the executor instead of blocking its worker.
    async fn enqueue_signal_marker_with_retry_async(
        &self,
        workflow_pid: Pid,
        marker: Atom,
    ) -> Result<(), EngineError> {
        let attempts = self.signal_delivery.max_enqueue_attempts.max(1);
        let mut backoff = self.signal_delivery.initial_backoff;
        for attempt in 1..=attempts {
            if self.scheduler.enqueue_atom_message(workflow_pid, marker) {
                self.confirm_marker_wake(workflow_pid);
                return Ok(());
            }

            if self.scheduler.process_table().get(workflow_pid).is_none() {
                return Err(runtime_error(format!(
                    "failed to deliver signal to workflow process {workflow_pid}: process is not live"
                )));
            }

            if attempt < attempts {
                // Same transient-window rationale as the blocking variant.
                yield_signal_delivery_backoff(backoff).await;
                backoff = next_signal_delivery_backoff(backoff, self.signal_delivery.max_backoff);
            }
        }

        Err(runtime_error(format!(
            "failed to deliver signal to workflow process {workflow_pid} after {attempts} attempts"
        )))
    }

    /// Arm the consumption-gated wake ladder for a delivered marker.
    ///
    /// `enqueue_atom_message` stores the message and wakes the pid, but
    /// beamr 0.4.9's `Wait`-arm gap can swallow that wake (the message is
    /// stored after the parked process's mailbox re-check and the wake runs
    /// before its wait-set insert), parking the process forever on a
    /// one-shot delivery. Follow-up wakes land after the insert and drain
    /// the already-stored message; the ladder stops once the target's
    /// wake-observation epoch moves — a suspending-native entry or process
    /// exit after this delivery — so it survives arbitrarily stretched gaps
    /// (OS preemption) without waking healthy processes forever.
    fn confirm_marker_wake(&self, workflow_pid: Pid) {
        let state = std::sync::Arc::clone(self.nif_state());
        let snapshot = state.wake_observation_epoch(workflow_pid);
        self.wake_confirmer
            .confirm(self.scheduler.wake_notifier(workflow_pid), move || {
                state.wake_ladder_done(workflow_pid, snapshot)
            });
    }
}

fn activity_failure(message: String) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message,
        details: None,
    }
}

fn correlation_to_activity_pid(correlation_id: &str) -> Result<Pid, EngineError> {
    let Some(raw) = correlation_id.strip_prefix("activity:") else {
        return Err(runtime_error(format!(
            "invalid activity correlation id {correlation_id}"
        )));
    };
    raw.parse::<Pid>().map_err(|error| {
        runtime_error(format!(
            "invalid activity correlation sequence {correlation_id}: {error}"
        ))
    })
}

fn next_signal_delivery_backoff(
    current: std::time::Duration,
    max: std::time::Duration,
) -> std::time::Duration {
    let doubled = current.saturating_mul(2);
    if doubled > max { max } else { doubled }
}

fn sleep_signal_delivery_backoff(duration: std::time::Duration) {
    if duration.is_zero() {
        std::thread::yield_now();
    } else {
        std::thread::sleep(duration);
    }
}

async fn yield_signal_delivery_backoff(duration: std::time::Duration) {
    if duration.is_zero() {
        tokio::task::yield_now().await;
    } else {
        tokio::time::sleep(duration).await;
    }
}
