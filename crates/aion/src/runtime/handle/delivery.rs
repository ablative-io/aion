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

    fn enqueue_signal_marker_with_retry(
        &self,
        workflow_pid: Pid,
        marker: Atom,
    ) -> Result<(), EngineError> {
        let attempts = self.signal_delivery.max_enqueue_attempts.max(1);
        let mut backoff = self.signal_delivery.initial_backoff;
        for attempt in 1..=attempts {
            if self.scheduler.enqueue_atom_message(workflow_pid, marker) {
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
