//! Mailbox delivery surface of [`RuntimeHandle`]: wake markers, two-phase
//! activity completion retention, and the retry-tolerant enqueue path.
//!
//! Markers are pure wakes — durable state lives in recorded history or the
//! retained completion maps, never in the marker itself.

use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload, RunId, WorkflowId,
};
use beamr::atom::Atom;
use beamr::process::ExitReason;

use crate::error::EngineError;
use crate::registry::Registry;

use super::activity_delivery::{ActivityOutcomeKind, RetainedActivityDelivery};
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
        let observed = self.activity_process_exit_outcome(activity_pid)?;
        self.release_spawn_heaps(activity_pid);
        if observed.reason == ExitReason::Normal {
            let payload = term_to_payload(observed.result.root(), &self.atom_table)?;
            self.deliver_activity_result(parent_pid, activity_pid, payload)
        } else {
            let error = self
                .activity_errors
                .get(&(parent_pid, activity_pid))
                .map_or_else(
                    || ActivityError {
                        kind: ActivityErrorKind::Terminal,
                        message: activity_exit_message(activity_pid, observed.reason),
                        details: None,
                    },
                    |entry| entry.clone(),
                );
            self.deliver_activity_error(parent_pid, activity_pid, error)
        }
    }

    /// Block until an in-VM activity child exits and decode its outcome.
    ///
    /// The child body is the SDK-composed runner thunk, whose Gleam `Result`
    /// crosses the exit boundary verbatim: a `Normal` exit carrying
    /// `{ok, JsonBin}` is a completion, `{error, ReasonBin}` is a failure
    /// whose reason already uses the SDK's prefixed vocabulary
    /// (`retryable:`/`terminal:`/...), and an abnormal exit (runner panic,
    /// `let assert`, NIF badarg) synthesizes a `terminal:`-prefixed reason
    /// mirroring [`Self::propagate_activity_outcome`]'s trapped-exit message.
    /// A `Normal` exit with any other result shape is a defect surfaced as a
    /// terminal failure, never a hang.
    ///
    /// Deliberately NOT keyed through the legacy `(parent, child_pid)` maps:
    /// the caller delivers the decoded outcome by correlation id into the
    /// ordinal-keyed two-phase maps, the same regime the remote wire uses.
    pub(crate) fn in_vm_child_outcome(
        &self,
        child_pid: Pid,
    ) -> Result<InVmChildOutcome, EngineError> {
        let observed = self.activity_process_exit_outcome(child_pid)?;
        self.release_spawn_heaps(child_pid);
        if observed.reason == ExitReason::Normal {
            match decode_in_vm_result(observed.result.root()) {
                Some(outcome) => Ok(outcome),
                None => Ok(InVmChildOutcome::Failed(format!(
                    "terminal:activity process {child_pid} returned an unexpected result shape"
                ))),
            }
        } else {
            Ok(InVmChildOutcome::Failed(format!(
                "terminal:{}",
                activity_exit_message(child_pid, observed.reason)
            )))
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
    /// Returns [`EngineError::ActivityDeliveryPoisoned`] when this workflow's
    /// scoped delivery gate was poisoned, or [`EngineError::Runtime`] when the
    /// workflow is not live or the marker cannot be queued.
    pub(crate) fn deliver_activity_completion_message(
        &self,
        workflow_pid: Pid,
        correlation_id: &str,
        result: String,
    ) -> Result<(), EngineError> {
        self.deliver_activity_completion_message_with_attempt(
            workflow_pid,
            correlation_id,
            result,
            None,
        )
    }

    pub(crate) fn deliver_activity_completion_message_with_attempt(
        &self,
        workflow_pid: Pid,
        correlation_id: &str,
        result: String,
        attempt: Option<u32>,
    ) -> Result<(), EngineError> {
        let activity_id = correlation_to_activity_pid(correlation_id)?;
        let key = (workflow_pid, activity_id);
        let marker = self.atom_table.intern("activity_complete");
        self.retain_activity_outcome_and_deliver_marker(
            workflow_pid,
            &self.activity_results,
            RetainedActivityDelivery {
                key,
                outcome: Payload::new(ContentType::Json, result.into_bytes()),
                kind: ActivityOutcomeKind::Result,
                attempt,
            },
            || self.enqueue_activity_marker(workflow_pid, marker, activity_id, correlation_id),
        )
    }

    /// Deliver a two-phase activity failure marker to the workflow mailbox.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ActivityDeliveryPoisoned`] when this workflow's
    /// scoped delivery gate was poisoned, or [`EngineError::Runtime`] when the
    /// workflow is not live or the marker cannot be queued.
    pub(crate) fn deliver_activity_failure_message(
        &self,
        workflow_pid: Pid,
        correlation_id: &str,
        reason: String,
    ) -> Result<(), EngineError> {
        self.deliver_activity_failure_message_with_attempt(
            workflow_pid,
            correlation_id,
            reason,
            None,
        )
    }

    pub(crate) fn deliver_activity_failure_message_with_attempt(
        &self,
        workflow_pid: Pid,
        correlation_id: &str,
        reason: String,
        attempt: Option<u32>,
    ) -> Result<(), EngineError> {
        let activity_id = correlation_to_activity_pid(correlation_id)?;
        let key = (workflow_pid, activity_id);
        let marker = self.atom_table.intern("activity_failed");
        self.retain_activity_outcome_and_deliver_marker(
            workflow_pid,
            &self.activity_errors,
            RetainedActivityDelivery {
                key,
                outcome: activity_failure(reason),
                kind: ActivityOutcomeKind::Error,
                attempt,
            },
            || self.enqueue_activity_marker(workflow_pid, marker, activity_id, correlation_id),
        )
    }

    /// Route an unmatched durable-outbox activity completion into the live
    /// workflow's mailbox.
    ///
    /// Resolves `workflow_id` to its live pid through `registry` (the
    /// [`RuntimeHandle`] does not hold the registry) and delegates to
    /// [`Self::deliver_activity_completion_message`], whose retained payload
    /// the engine's `take_and_record` later records as the terminal.
    ///
    /// Returns `Ok(true)` when delivered to a live workflow and `Ok(false)`
    /// when no run for the workflow is currently live — the expected
    /// stale-completion case after a crash or eviction, which recovery
    /// re-arms. A `false` is not an error: the caller logs it at debug.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] when the registry index lock
    /// was poisoned, [`EngineError::ActivityDeliveryPoisoned`] when the
    /// resolved workflow's scoped delivery gate was poisoned, or
    /// [`EngineError::Runtime`] when the process is not live or the mailbox
    /// marker cannot be queued.
    pub fn deliver_outbox_completion(
        &self,
        registry: &Registry,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        result: String,
    ) -> Result<bool, EngineError> {
        // Run-aware gate: a completion carrying a run_id is only delivered when
        // that run is still the workflow's live run. After continue-as-new the
        // prior run is superseded, and its late completion must NOT resolve the
        // new run's reused ordinal (OBX-011). The recorder's
        // `record_fan_out_completion` run check is the second enforcement layer.
        let Some(pid) = outbox_delivery_pid(registry, workflow_id, run_id)? else {
            return Ok(false);
        };
        self.deliver_activity_completion_message(pid, &activity_id.to_string(), result)?;
        Ok(true)
    }

    /// Route an unmatched durable-outbox activity failure into the live
    /// workflow's mailbox.
    ///
    /// Failure twin of [`Self::deliver_outbox_completion`]: same registry
    /// resolution and the same not-live `Ok(false)` outcome, delegating to
    /// [`Self::deliver_activity_failure_message`].
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] when the registry index lock
    /// was poisoned, [`EngineError::ActivityDeliveryPoisoned`] when the
    /// resolved workflow's scoped delivery gate was poisoned, or
    /// [`EngineError::Runtime`] when the process is not live or the mailbox
    /// marker cannot be queued.
    pub fn deliver_outbox_failure(
        &self,
        registry: &Registry,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        reason: String,
    ) -> Result<bool, EngineError> {
        // Run-aware gate, identical to `deliver_outbox_completion`: a failure
        // belonging to a superseded run (post continue-as-new) must not resolve
        // the new run's reused ordinal (OBX-011).
        let Some(pid) = outbox_delivery_pid(registry, workflow_id, run_id)? else {
            return Ok(false);
        };
        self.deliver_activity_failure_message(pid, &activity_id.to_string(), reason)?;
        Ok(true)
    }

    /// Deliver a successful activity result payload to the workflow mailbox surface.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ActivityDeliveryPoisoned`] when the parent's
    /// scoped delivery gate was poisoned, or [`EngineError::Runtime`] when the
    /// workflow is not live or the mailbox marker cannot be queued.
    pub fn deliver_activity_result(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
        payload: Payload,
    ) -> Result<(), EngineError> {
        let key = (parent_pid, activity_pid);
        let marker = self.atom_table.intern("aion_activity_result");
        self.retain_activity_outcome_and_deliver_marker(
            parent_pid,
            &self.activity_results,
            RetainedActivityDelivery {
                key,
                outcome: payload,
                kind: ActivityOutcomeKind::Result,
                attempt: None,
            },
            || {
                self.enqueue_activity_marker(
                    parent_pid,
                    marker,
                    activity_pid,
                    &format!("activity process {activity_pid}"),
                )
            },
        )
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

    /// Store a typed activity error for a trapped activity EXIT signal.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ActivityDeliveryPoisoned`] when the parent's
    /// scoped delivery gate was poisoned, or [`EngineError::Runtime`] when the
    /// workflow process is not live.
    pub fn deliver_activity_error(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
        error: ActivityError,
    ) -> Result<(), EngineError> {
        self.with_activity_delivery(parent_pid, |state| {
            self.ensure_activity_delivery_live(parent_pid, state)?;
            self.activity_errors
                .insert((parent_pid, activity_pid), error);
            state.retain_outcome(activity_pid, ActivityOutcomeKind::Error);
            Ok(())
        })
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
    ) -> Result<Option<(Payload, Option<u32>)>, EngineError> {
        self.take_activity_outcome(
            parent_pid,
            activity_sequence,
            &self.activity_results,
            ActivityOutcomeKind::Result,
        )
    }

    pub(crate) fn take_activity_error(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
    ) -> Result<Option<(ActivityError, Option<u32>)>, EngineError> {
        self.take_activity_outcome(
            parent_pid,
            activity_sequence,
            &self.activity_errors,
            ActivityOutcomeKind::Error,
        )
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
    /// beamr's `Wait`-arm gap can swallow that wake (the message is
    /// stored after the parked process's mailbox re-check and the wake runs
    /// before its wait-set insert), parking the process forever on a
    /// one-shot delivery. Follow-up wakes land after the insert and drain
    /// the already-stored message; the ladder stops once the target's
    /// wake-observation epoch moves — a suspending-native entry or process
    /// exit after this delivery — so it survives arbitrarily stretched gaps
    /// (OS preemption) without waking healthy processes forever.
    ///
    /// NOTE: this workaround was written against beamr 0.4.9. The crate is now
    /// pinned to beamr 0.6.4; the `Wait`-arm gap may have been fixed upstream,
    /// so this ladder needs re-validation against 0.6.4 and may now be stale.
    pub(super) fn confirm_marker_wake(&self, workflow_pid: Pid) {
        let state = std::sync::Arc::clone(self.nif_state());
        let snapshot = state.wake_observation_epoch(workflow_pid);
        self.wake_confirmer
            .confirm(self.scheduler.wake_notifier(workflow_pid), move || {
                state.wake_ladder_done(workflow_pid, snapshot)
            });
    }
}

/// Resolve the pid an unmatched outbox completion/failure should be delivered
/// to, enforcing run scoping when a `run_id` is supplied.
///
/// When `run_id` is `Some(r)`, delivery is gated on the workflow's live run
/// still being `r`: a completion for a superseded/dead run (e.g. a prior run
/// after continue-as-new) resolves to `Ok(None)` and is dropped, so it can
/// never resolve the new run's reused ordinal space (OBX-011).
///
/// When `run_id` is `None` (legacy/pre-CAN callers), this preserves the
/// original run-agnostic behaviour: deliver to whatever run is live.
///
/// `Ok(None)` is the not-live / wrong-run outcome, never an error.
fn outbox_delivery_pid(
    registry: &Registry,
    workflow_id: &WorkflowId,
    run_id: Option<&RunId>,
) -> Result<Option<u64>, EngineError> {
    match run_id {
        None => registry.live_pid(workflow_id),
        Some(expected) => {
            let Some((live_run, pid)) = registry.live_run_pid(workflow_id)? else {
                return Ok(None);
            };
            if live_run == *expected {
                Ok(Some(pid))
            } else {
                tracing::debug!(
                    %workflow_id,
                    %expected,
                    live_run = %live_run,
                    "dropping outbox delivery for superseded run"
                );
                Ok(None)
            }
        }
    }
}

fn activity_failure(message: String) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message,
        details: None,
    }
}

/// The one canonical message for an activity child that exited abnormally,
/// shared by the trapped-exit propagation path and the in-VM outcome decode.
fn activity_exit_message(activity_pid: Pid, reason: ExitReason) -> String {
    format!("activity process {activity_pid} exited: {reason:?}")
}

/// Outcome of one in-VM activity child, decoded at its exit boundary.
///
/// Both variants carry the raw wire string the correlation-keyed delivery
/// path expects: a completion carries the runner's output-codec JSON, a
/// failure carries the SDK's prefixed reason vocabulary.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InVmChildOutcome {
    /// Normal exit with `{ok, JsonBin}`: the encoded activity output.
    Completed(String),
    /// Normal exit with `{error, ReasonBin}`, or a synthesized reason for an
    /// abnormal exit / unexpected result shape.
    Failed(String),
}

/// Decode the thunk child's exit result term (`{ok, Bin} | {error, Bin}`).
///
/// Returns `None` for any other shape — including non-UTF-8 payload bytes —
/// so the caller synthesizes a terminal failure instead of guessing.
fn decode_in_vm_result(term: beamr::term::Term) -> Option<InVmChildOutcome> {
    let tuple = beamr::term::boxed::Tuple::new(term)?;
    if tuple.arity() != 2 {
        return None;
    }
    let tag = tuple.get(0)?;
    let value = tuple.get(1)?;
    let bin = beamr::term::binary_ref::BinaryRef::new(value)?;
    let text = String::from_utf8(bin.as_bytes().to_vec()).ok()?;
    if tag == beamr::term::Term::atom(Atom::OK) {
        Some(InVmChildOutcome::Completed(text))
    } else if tag == beamr::term::Term::atom(Atom::ERROR) {
        Some(InVmChildOutcome::Failed(text))
    } else {
        None
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

pub(super) fn next_signal_delivery_backoff(
    current: std::time::Duration,
    max: std::time::Duration,
) -> std::time::Duration {
    let doubled = current.saturating_mul(2);
    if doubled > max { max } else { doubled }
}

pub(super) fn sleep_signal_delivery_backoff(duration: std::time::Duration) {
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

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
