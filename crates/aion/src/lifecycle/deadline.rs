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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::WorkflowDeadlineHandler;
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, TerminalOutcome, WorkflowHandle,
        WorkflowHandleParts,
    };
    use crate::runtime::{RuntimeConfig, RuntimeHandle};
    use crate::time::DeadlineHandler;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A registered, running workflow with a live process, plus the deadline
    /// handler wired to its store, visibility index, and registry.
    struct TimedRun {
        handler: WorkflowDeadlineHandler,
        store: Arc<dyn EventStore>,
        registry: Arc<Registry>,
        runtime: Arc<RuntimeHandle>,
        handle: WorkflowHandle,
    }

    async fn timed_run() -> Result<TimedRun, Box<dyn std::error::Error>> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let registry = Arc::new(Registry::default());
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&store));
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                crate::durability::WorkflowStartRecord {
                    workflow_type: "sleeper".to_owned(),
                    input: Payload::from_json(&json!({}))?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        let pid = runtime.spawn_test_process_with_trap_exit(true)?;
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "sleeper".to_owned(),
            namespace: String::from("default"),
            loaded_version: ContentHash::from_bytes([7; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle.clone())?;
        let handler = WorkflowDeadlineHandler::new(
            Arc::downgrade(&runtime),
            Arc::clone(&store),
            Arc::clone(&visibility_store),
            Arc::clone(&registry),
        );
        Ok(TimedRun {
            handler,
            store,
            registry,
            runtime,
            handle,
        })
    }

    /// No concurrent terminal: the elapsed deadline records `WorkflowTimedOut`
    /// with the `"workflow"` descriptor, projects `TimedOut`, notifies the
    /// awaiter, and deregisters the run.
    #[tokio::test(flavor = "multi_thread")]
    async fn deadline_records_timed_out_and_tears_down() -> TestResult {
        let run = timed_run().await?;
        let workflow_id = run.handle.workflow_id().clone();
        let run_id = run.handle.run_id().clone();
        let mut receiver = run.handle.completion().subscribe();

        run.handler
            .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
            .await?;
        receiver.changed().await?;

        let history = run.store.read_history(&workflow_id).await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowTimedOut { timeout, .. },
            ] => assert_eq!(timeout, "workflow"),
            other => return Err(format!("expected started then timed out, found {other:?}").into()),
        }
        assert_eq!(
            aion_core::status_from_events(&history),
            WorkflowStatus::TimedOut
        );
        assert_eq!(
            receiver.borrow().clone(),
            Some(TerminalOutcome::TimedOut(String::from("workflow")))
        );
        assert_eq!(run.registry.get(&workflow_id, &run_id)?, None);
        run.runtime.shutdown()?;
        Ok(())
    }

    /// Deadline-vs-completion race, resolved under the recorder lock: a terminal
    /// already recorded for the run makes the elapsed deadline a clean no-op — it
    /// records NO `WorkflowTimedOut` and leaves the prior terminal intact.
    #[tokio::test(flavor = "multi_thread")]
    async fn deadline_loses_to_an_already_recorded_terminal() -> TestResult {
        let run = timed_run().await?;
        let workflow_id = run.handle.workflow_id().clone();
        let run_id = run.handle.run_id().clone();

        // The run completes first (the race the deadline must lose).
        {
            let recorder = run.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), Payload::from_json(&json!("done"))?)
                .await?;
        }

        run.handler
            .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
            .await?;

        let history = run.store.read_history(&workflow_id).await?;
        assert!(
            !history
                .iter()
                .any(|event| matches!(event, Event::WorkflowTimedOut { .. })),
            "the losing deadline must record no WorkflowTimedOut: {history:#?}"
        );
        assert_eq!(
            aion_core::status_from_events(&history),
            WorkflowStatus::Completed,
            "the concurrent completion stands"
        );
        run.runtime.shutdown()?;
        Ok(())
    }

    /// A deadline for a run that already left the registry (a terminal
    /// deregistered it) is a clean no-op — nothing recorded.
    #[tokio::test(flavor = "multi_thread")]
    async fn deadline_for_deregistered_run_is_a_noop() -> TestResult {
        let run = timed_run().await?;
        let workflow_id = run.handle.workflow_id().clone();
        let run_id = run.handle.run_id().clone();
        run.registry.remove(&workflow_id, &run_id)?;

        run.handler
            .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
            .await?;

        let history = run.store.read_history(&workflow_id).await?;
        assert!(
            !history
                .iter()
                .any(|event| matches!(event, Event::WorkflowTimedOut { .. })),
            "a deregistered run's deadline records nothing: {history:#?}"
        );
        run.runtime.shutdown()?;
        Ok(())
    }
}
