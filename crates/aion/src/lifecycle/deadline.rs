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
                self.tear_down(&handle, &workflow_id, &run_id).await;
                Ok(())
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
    /// durable. Every step is independent and re-runnable: retire the run's
    /// active timers, kill the process, refresh visibility, notify awaiters, and
    /// deregister. One failing step is logged and never suppresses the rest, so an
    /// interrupted teardown that a later re-fire resumes eventually completes.
    async fn tear_down(&self, handle: &WorkflowHandle, workflow_id: &WorkflowId, run_id: &RunId) {
        // Retire the run's still-live timers (the deadline itself and any parked
        // sleeps) BEFORE deregistration, so recovery does not rediscover them and
        // a late fire is refused. Recording is idempotent: a re-run finds them
        // already retired and records nothing.
        self.retire_active_timers(handle, workflow_id).await;

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
                tracing::warn!(
                    %workflow_id,
                    %run_id,
                    "runtime dropped during deadline teardown; timeout is recorded and a later re-fire resumes teardown"
                );
            }
        }

        if let Err(error) = upsert_workflow_visibility(
            Arc::clone(&self.store),
            Arc::clone(&self.visibility_store),
            workflow_id,
            run_id,
        )
        .await
        {
            tracing::warn!(
                %workflow_id,
                %run_id,
                %error,
                "failed to refresh visibility during deadline teardown; a later re-fire resumes it"
            );
        }

        handle.completion().notify(TerminalOutcome::TimedOut(
            WORKFLOW_TIMEOUT_DESCRIPTOR.to_owned(),
        ));

        if let Err(error) = self.registry.remove(workflow_id, run_id) {
            tracing::warn!(
                %workflow_id,
                %run_id,
                %error,
                "failed to deregister timed-out run; a later re-fire resumes it"
            );
        }
    }

    /// Retires the timed-out run's still-live timers by recording
    /// `TimerCancelled { WorkflowIntent }` for each, through the handle recorder
    /// under its lock. Serialized against concurrent fires by that same recorder
    /// lock, and idempotent — a re-run sees no live timers and records nothing.
    async fn retire_active_timers(&self, handle: &WorkflowHandle, workflow_id: &WorkflowId) {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        let history = match self.store.read_history(workflow_id).await {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    %workflow_id,
                    %error,
                    "could not read history to retire timers during deadline teardown"
                );
                return;
            }
        };
        for timer_id in live_timers_in_active_segment(&history) {
            if let Err(error) = recorder
                .record_timer_cancelled(
                    Utc::now(),
                    timer_id.clone(),
                    TimerCancelCause::WorkflowIntent,
                )
                .await
            {
                tracing::warn!(
                    %workflow_id,
                    %timer_id,
                    %error,
                    "failed to retire a timer during deadline teardown; recovery will skip it if orphaned"
                );
            }
        }
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
