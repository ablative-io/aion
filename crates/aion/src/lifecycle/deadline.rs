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
        visibility_store: Arc<dyn VisibilityStore>,
        registry: Arc<Registry>,
        runtime: Arc<RuntimeHandle>,
        handle: WorkflowHandle,
    }

    /// Records a deadline `TimerStarted` for the run so its history looks armed.
    async fn arm_deadline(run: &TimedRun) -> Result<(), Box<dyn std::error::Error>> {
        let deadline_id = crate::time::deadline_timer_id(run.handle.run_id())?;
        let recorder = run.handle.recorder();
        let mut recorder = recorder.lock().await;
        recorder
            .record_timer_started(chrono::Utc::now(), deadline_id, chrono::Utc::now())
            .await?;
        Ok(())
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
            visibility_store,
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
        // A fired deadline is, by construction, an armed one: its `TimerStarted`
        // is in history. The handler re-checks that liveness, so the test must
        // arm it just as the start path does.
        arm_deadline(&run).await?;
        let mut receiver = run.handle.completion().subscribe();

        run.handler
            .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
            .await?;
        receiver.changed().await?;

        let history = run.store.read_history(&workflow_id).await?;
        assert_eq!(
            count_timed_out(&history),
            1,
            "one timeout terminal: {history:#?}"
        );
        match history
            .iter()
            .find(|event| matches!(event, Event::WorkflowTimedOut { .. }))
        {
            Some(Event::WorkflowTimedOut { timeout, .. }) => assert_eq!(timeout, "workflow"),
            _ => return Err("no WorkflowTimedOut recorded".into()),
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

    fn count_timed_out(history: &[Event]) -> usize {
        history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowTimedOut { .. }))
            .count()
    }

    /// Teardown retires the run's active timers: an elapsed deadline records
    /// `WorkflowTimedOut`, then cancels its own deadline timer AND a parked sleep
    /// (both `TimerCancelled`), so recovery finds no live timer to rediscover.
    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_teardown_retires_the_runs_active_timers() -> TestResult {
        let run = timed_run().await?;
        let workflow_id = run.handle.workflow_id().clone();
        let run_id = run.handle.run_id().clone();
        arm_deadline(&run).await?;
        // A parked author sleep that must be retired by teardown.
        let sleep_id = aion_core::TimerId::named("nap")?;
        {
            let recorder = run.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_timer_started(chrono::Utc::now(), sleep_id.clone(), chrono::Utc::now())
                .await?;
        }

        run.handler
            .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
            .await?;

        let history = run.store.read_history(&workflow_id).await?;
        assert_eq!(count_timed_out(&history), 1);
        let deadline_id = crate::time::deadline_timer_id(&run_id)?;
        assert!(
            crate::time::timer_service::live_timers_in_active_segment(&history).is_empty(),
            "teardown retires every active-run timer: {history:#?}"
        );
        let cancelled: Vec<&aion_core::TimerId> = history
            .iter()
            .filter_map(|event| match event {
                Event::TimerCancelled { timer_id, .. } => Some(timer_id),
                _ => None,
            })
            .collect();
        assert!(cancelled.contains(&&deadline_id), "the deadline is retired");
        assert!(
            cancelled.contains(&&sleep_id),
            "the parked sleep is retired"
        );
        assert_eq!(run.registry.get(&workflow_id, &run_id)?, None);
        run.runtime.shutdown()?;
        Ok(())
    }

    /// A deadline whose timer was already retired (`TimerCancelled`) — a cancel
    /// that recorded its intent before its terminal — loses cleanly: no
    /// `WorkflowTimedOut`, even though no workflow terminal is present yet.
    #[tokio::test(flavor = "multi_thread")]
    async fn retired_deadline_loses_before_any_workflow_terminal() -> TestResult {
        let run = timed_run().await?;
        let workflow_id = run.handle.workflow_id().clone();
        let run_id = run.handle.run_id().clone();
        arm_deadline(&run).await?;
        let deadline_id = crate::time::deadline_timer_id(&run_id)?;
        {
            let recorder = run.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_timer_cancelled(
                    chrono::Utc::now(),
                    deadline_id,
                    aion_core::TimerCancelCause::WorkflowIntent,
                )
                .await?;
        }

        run.handler
            .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
            .await?;

        let history = run.store.read_history(&workflow_id).await?;
        assert_eq!(
            count_timed_out(&history),
            0,
            "a retired deadline never times the run out: {history:#?}"
        );
        run.runtime.shutdown()?;
        Ok(())
    }

    /// Idempotent, resumable teardown: an already-recorded `WorkflowTimedOut`
    /// whose teardown was interrupted (the run is still registered) is resumed by
    /// a re-fire WITHOUT a second terminal — the run is deregistered.
    #[tokio::test(flavor = "multi_thread")]
    async fn re_fire_after_recorded_timeout_resumes_teardown_without_a_second_terminal()
    -> TestResult {
        let run = timed_run().await?;
        let workflow_id = run.handle.workflow_id().clone();
        let run_id = run.handle.run_id().clone();
        arm_deadline(&run).await?;
        // The terminal is durable but the run was NOT deregistered (teardown was
        // interrupted after the append).
        {
            let recorder = run.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_timed_out(chrono::Utc::now(), String::from("workflow"))
                .await?;
        }
        assert!(run.registry.get(&workflow_id, &run_id)?.is_some());

        run.handler
            .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
            .await?;

        let history = run.store.read_history(&workflow_id).await?;
        assert_eq!(
            count_timed_out(&history),
            1,
            "resuming teardown records no second WorkflowTimedOut: {history:#?}"
        );
        assert_eq!(
            run.registry.get(&workflow_id, &run_id)?,
            None,
            "resumed teardown deregisters the run"
        );
        run.runtime.shutdown()?;
        Ok(())
    }

    /// The real deadline handler raced against the real cancel path (both guard
    /// the terminal under the recorder lock) yields EXACTLY ONE terminal, whoever
    /// wins. Mutation-sensitive: moving either check outside the lock admits a
    /// double terminal. Repeated to stress the interleaving.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn deadline_and_cancel_race_records_exactly_one_terminal() -> TestResult {
        for _ in 0..40 {
            let run = timed_run().await?;
            let workflow_id = run.handle.workflow_id().clone();
            let run_id = run.handle.run_id().clone();
            arm_deadline(&run).await?;

            let handler_wf = workflow_id.clone();
            let handler_run = run_id.clone();
            let deadline = async {
                run.handler
                    .on_deadline_elapsed(handler_wf, handler_run)
                    .await
            };
            let cancel = crate::lifecycle::terminate::cancel(
                crate::lifecycle::terminate::TerminateWorkflowContext {
                    runtime: run.runtime.as_ref(),
                    store: Arc::clone(&run.store),
                    visibility_store: Arc::clone(&run.visibility_store),
                    registry: run.registry.as_ref(),
                },
                &workflow_id,
                &run_id,
                "operator cancel",
            );
            let (deadline_result, cancel_result) = tokio::join!(deadline, cancel);
            deadline_result?;
            // The cancel may lose the registry lookup race (the deadline already
            // deregistered); that typed not-found is a legitimate loss.
            if let Err(error) = cancel_result {
                assert!(
                    matches!(error, crate::EngineError::WorkflowNotFound { .. }),
                    "cancel lost the race cleanly, got {error:?}"
                );
            }

            let history = run.store.read_history(&workflow_id).await?;
            let terminals = history
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::WorkflowTimedOut { .. } | Event::WorkflowCancelled { .. }
                    )
                })
                .count();
            assert_eq!(terminals, 1, "exactly one terminal wins: {history:#?}");
            run.runtime.shutdown()?;
        }
        Ok(())
    }
}
