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

use aion_core::{RunId, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;

use crate::registry::{Registry, TerminalOutcome};
use crate::runtime::RuntimeHandle;
use crate::time::{DeadlineHandler, DeadlineHandlerError, WORKFLOW_TIMEOUT_DESCRIPTOR};

use super::completion::terminal_outcome_from_history;
use super::visibility::upsert_workflow_visibility;

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
            // deregistered it). The deadline lost the race: nothing to record.
            tracing::debug!(
                %workflow_id,
                %run_id,
                "workflow deadline elapsed but the run is no longer registered; a terminal already won"
            );
            return Ok(());
        };

        // Terminal re-check and the timeout record are atomic under the recorder
        // lock: a concurrent complete/fail/cancel records through the same
        // recorder, so a check outside the lock could double-record a terminal.
        {
            let recorder = handle.recorder();
            let mut recorder = recorder.lock().await;
            let history = self.store.read_history(&workflow_id).await?;
            if terminal_outcome_from_history(&history, &run_id).is_some() {
                tracing::debug!(
                    %workflow_id,
                    %run_id,
                    "workflow deadline elapsed but a terminal was already recorded; deadline loses"
                );
                return Ok(());
            }
            recorder
                .record_workflow_timed_out(Utc::now(), WORKFLOW_TIMEOUT_DESCRIPTOR.to_owned())
                .await?;
        }

        // Kill the process AFTER releasing the recorder lock, mirroring
        // `terminate::cancel`: the exit monitor records through the same recorder
        // and would otherwise deadlock against a still-held lock. A miss here (the
        // process already exited) is reconciled against the recorded timeout.
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
                // The engine is shutting down; the recorded terminal stands and
                // recovery reconciles the rest on the next owner.
                tracing::warn!(
                    %workflow_id,
                    %run_id,
                    "runtime dropped during deadline teardown; timeout is recorded but the process was not killed here"
                );
            }
        }

        upsert_workflow_visibility(
            Arc::clone(&self.store),
            Arc::clone(&self.visibility_store),
            &workflow_id,
            &run_id,
        )
        .await?;

        handle.completion().notify(TerminalOutcome::TimedOut(
            WORKFLOW_TIMEOUT_DESCRIPTOR.to_owned(),
        ));
        self.registry.remove(&workflow_id, &run_id)?;
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
