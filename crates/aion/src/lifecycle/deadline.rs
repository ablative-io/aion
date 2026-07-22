//! Engine-side handler that drives an elapsed workflow deadline to a
//! `WorkflowTimedOut` terminal.
//!
//! Registered on the timer bridge at engine construction, this is the seam the
//! `TimerService` demuxes a reserved `deadline:{run_id}` fire to. It records the
//! terminal under the per-handle recorder lock — with a terminal re-check so it
//! loses cleanly to a concurrent completion — then tears the run down matching
//! `terminate::cancel` discipline: kill the process, refresh visibility, notify
//! result awaiters, and deregister.
//!
//! It holds a `Weak<RuntimeHandle>` (never a strong one) so the engine's
//! `RuntimeHandle` → `EngineNifState` → timer bridge → handler chain does not
//! cycle back into the runtime — the same cycle-avoidance the timer bridge's
//! `Weak<EngineNifState>` observes.

use std::sync::{Arc, Weak};

use aion_core::{RunId, TimerCancelCause, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;

use crate::registry::{Registry, TerminalOutcome, WorkflowHandle};
use crate::runtime::RuntimeHandle;
use crate::time::timer_service::live_timers_in_active_segment;
use crate::time::{DeadlineHandler, DeadlineHandlerError, WORKFLOW_TIMEOUT_DESCRIPTOR};

use super::completion::terminal_outcome_from_history;
use super::visibility::upsert_workflow_visibility;

/// Whether the elapsed deadline records a fresh terminal, resumes an interrupted
/// teardown of its own prior terminal, or loses cleanly to a competing terminal.
enum DeadlineDisposition {
    /// This call appended `WorkflowTimedOut`; run the full teardown.
    Appended,
    /// Our own `WorkflowTimedOut` is already durable but teardown was
    /// interrupted; resume the idempotent teardown without a second terminal.
    ResumeTeardown,
    /// A competing terminal already won (or the deadline is no longer live);
    /// nothing to record and nothing to tear down.
    LoseCleanly,
}

/// Records `WorkflowTimedOut` and tears down a run whose deadline elapsed.
pub struct WorkflowDeadlineHandler {
    /// Weak to avoid the `RuntimeHandle`↔`EngineNifState`↔bridge cycle; upgraded
    /// only to kill the timed-out process.
    runtime: Weak<RuntimeHandle>,
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    registry: Arc<Registry>,
}

impl WorkflowDeadlineHandler {
    /// Assembles a deadline handler from the engine's teardown dependencies.
    ///
    /// `runtime` is held weakly on purpose (see the module docs); the rest are
    /// the same durable store, visibility index, and active registry the
    /// `terminate::cancel` path uses.
    #[must_use]
    pub fn new(
        runtime: Weak<RuntimeHandle>,
        store: Arc<dyn EventStore>,
        visibility_store: Arc<dyn VisibilityStore>,
        registry: Arc<Registry>,
    ) -> Self {
        Self {
            runtime,
            store,
            visibility_store,
            registry,
        }
    }

    /// Body of the timeout terminal + teardown, returning typed engine errors.
    async fn drive_timed_out(
        &self,
        workflow_id: WorkflowId,
        run_id: RunId,
    ) -> Result<(), crate::EngineError> {
        let Some(handle) = self.registry.get(&workflow_id, &run_id)? else {
            // The run already left the registry (a concurrent terminal won and
            // deregistered it, teardown complete). The deadline lost the race.
            tracing::debug!(
                %workflow_id,
                %run_id,
                "workflow deadline elapsed but the run is no longer registered; a terminal already won"
            );
            return Ok(());
        };

        let disposition = self
            .decide_disposition(&handle, &workflow_id, &run_id)
            .await?;
        match disposition {
            DeadlineDisposition::LoseCleanly => Ok(()),
            DeadlineDisposition::Appended | DeadlineDisposition::ResumeTeardown => {
                self.tear_down(&handle, &workflow_id, &run_id).await
            }
        }
    }

    /// Decides — atomically under the recorder lock — whether to append a fresh
    /// `WorkflowTimedOut`, resume an interrupted teardown of an already-recorded
    /// one, or lose cleanly.
    ///
    /// The terminal re-check, the deadline-liveness re-check, and the terminal
    /// append are one critical section: a concurrent complete/fail/cancel records
    /// through the same recorder, so checking outside the lock could double-record
    /// a terminal or let a cancelled deadline still time the run out.
    async fn decide_disposition(
        &self,
        handle: &WorkflowHandle,
        workflow_id: &WorkflowId,
        run_id: &RunId,
    ) -> Result<DeadlineDisposition, crate::EngineError> {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        let history = self.store.read_history(workflow_id).await?;
        match terminal_outcome_from_history(&history, run_id) {
            Some(TerminalOutcome::TimedOut(_)) => {
                // Our own terminal is durable but teardown did not finish (a
                // dropped runtime, a failed visibility upsert, an interrupted
                // fire). Resume the idempotent teardown — do NOT append again.
                tracing::debug!(
                    %workflow_id,
                    %run_id,
                    "workflow deadline re-fired after its WorkflowTimedOut was recorded; resuming teardown"
                );
                Ok(DeadlineDisposition::ResumeTeardown)
            }
            Some(_) => {
                // A competing terminal (complete/fail/cancel/continue-as-new) won.
                tracing::debug!(
                    %workflow_id,
                    %run_id,
                    "workflow deadline elapsed but another terminal was already recorded; deadline loses"
                );
                Ok(DeadlineDisposition::LoseCleanly)
            }
            None => {
                // Re-check THIS deadline is still live: a cancel that recorded
                // `TimerCancelled { WorkflowIntent }` before its terminal must win,
                // so a retired deadline loses cleanly rather than timing the run
                // out after its cancellation.
                if crate::time::outstanding_deadline_timer(&history, run_id).is_none() {
                    tracing::debug!(
                        %workflow_id,
                        %run_id,
                        "workflow deadline elapsed but its timer was already retired; deadline loses"
                    );
                    return Ok(DeadlineDisposition::LoseCleanly);
                }
                recorder
                    .record_workflow_timed_out(Utc::now(), WORKFLOW_TIMEOUT_DESCRIPTOR.to_owned())
                    .await?;
                Ok(DeadlineDisposition::Appended)
            }
        }
    }

    /// Idempotent, resumable teardown after the `WorkflowTimedOut` terminal is
    /// durable.
    ///
    /// Ordering is the invariant that makes resume reachable: the run's OWN
    /// deadline timer stays live and its registry entry stays present until every
    /// fallible teardown step has succeeded. So it retires the ordinary
    /// (non-deadline) timers first, confirms process teardown and refreshes
    /// visibility, notifies awaiters, and only THEN retires the deadline itself
    /// and deregisters. A failure in any earlier step is PROPAGATED (not merely
    /// logged): the handler returns it as a fire failure, the deadline remains
    /// live, and recovery's `outstanding_future_timers` re-arms it so a later fire
    /// re-enters here and resumes — rather than destroying both retry anchors
    /// before the work that needs them.
    ///
    /// # Errors
    ///
    /// Returns the typed [`crate::EngineError`] from the first failing durable
    /// step so recovery retries the interrupted teardown.
    async fn tear_down(
        &self,
        handle: &WorkflowHandle,
        workflow_id: &WorkflowId,
        run_id: &RunId,
    ) -> Result<(), crate::EngineError> {
        // 1. Retire the run's ordinary (non-deadline) timers. The deadline is
        //    deliberately NOT retired here — it is the resume anchor.
        self.retire_ordinary_timers(handle, workflow_id, run_id)
            .await?;

        // 2. Stop the timed-out process. A cancel failure means it already
        //    exited (benign); a dropped runtime is propagated so a re-fire under a
        //    live runtime completes the kill.
        match self.runtime.upgrade() {
            Some(runtime) => {
                if let Err(error) = runtime.cancel_pid(handle.pid()) {
                    tracing::debug!(
                        %workflow_id,
                        %run_id,
                        %error,
                        "workflow process already exited during deadline teardown"
                    );
                }
            }
            None => {
                return Err(crate::EngineError::Runtime {
                    reason: format!(
                        "runtime dropped during deadline teardown of {workflow_id}/{run_id}; a later re-fire resumes teardown"
                    ),
                });
            }
        }

        // 3. Refresh visibility; a failure is propagated so it is retried.
        upsert_workflow_visibility(
            Arc::clone(&self.store),
            Arc::clone(&self.visibility_store),
            workflow_id,
            run_id,
        )
        .await?;

        // 4. Notify awaiters (a doorbell send; never a retry condition).
        handle.completion().notify(TerminalOutcome::TimedOut(
            WORKFLOW_TIMEOUT_DESCRIPTOR.to_owned(),
        ));

        // 5. Retire the deadline LAST, once teardown has otherwise succeeded, so
        //    no earlier failure could have removed the resume anchor. Idempotent.
        self.retire_deadline(handle, workflow_id, run_id).await?;

        // 6. Deregister LAST.
        self.registry.remove(workflow_id, run_id)?;
        Ok(())
    }

    /// Retires the timed-out run's still-live ORDINARY timers (every live timer
    /// except this run's own deadline) by recording `TimerCancelled { WorkflowIntent }`
    /// for each, through the handle recorder under its lock. The deadline is
    /// excluded so it stays live as the teardown resume anchor. Idempotent — a
    /// re-run sees the same timers already retired and records nothing.
    ///
    /// # Errors
    ///
    /// Returns the typed [`crate::EngineError`] when history cannot be read or a
    /// cancellation append fails, so the interrupted teardown is retried.
    async fn retire_ordinary_timers(
        &self,
        handle: &WorkflowHandle,
        workflow_id: &WorkflowId,
        run_id: &RunId,
    ) -> Result<(), crate::EngineError> {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        let history = self.store.read_history(workflow_id).await?;
        let deadline = crate::time::outstanding_deadline_timer(&history, run_id);
        for timer_id in live_timers_in_active_segment(&history) {
            if deadline.as_ref() == Some(&timer_id) {
                continue;
            }
            recorder
                .record_timer_cancelled(Utc::now(), timer_id, TimerCancelCause::WorkflowIntent)
                .await?;
        }
        Ok(())
    }

    /// Retires this run's own declared-timeout deadline as the final teardown
    /// step, via the shared `retire_run_deadline` primitive. Idempotent — a
    /// resumed teardown whose deadline is already retired records nothing.
    ///
    /// # Errors
    ///
    /// Returns the typed [`crate::EngineError`] when history cannot be read or the
    /// cancellation append fails.
    async fn retire_deadline(
        &self,
        handle: &WorkflowHandle,
        workflow_id: &WorkflowId,
        run_id: &RunId,
    ) -> Result<(), crate::EngineError> {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        let history = self.store.read_history(workflow_id).await?;
        crate::time::retire_run_deadline(&mut recorder, &history, run_id).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl DeadlineHandler for WorkflowDeadlineHandler {
    async fn on_deadline_elapsed(
        &self,
        workflow_id: WorkflowId,
        run_id: RunId,
    ) -> Result<(), DeadlineHandlerError> {
        self.drive_timed_out(workflow_id, run_id)
            .await
            .map_err(|error| DeadlineHandlerError(error.to_string()))
    }
}

#[cfg(test)]
#[path = "deadline_tests.rs"]
mod tests;
