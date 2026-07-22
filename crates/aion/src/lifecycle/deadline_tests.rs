//! Tests for the [`super::WorkflowDeadlineHandler`] deadline terminal and
//! idempotent teardown behaviour.

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

/// A visibility store whose `record_visibility` errors while armed, then
/// delegates once disarmed — to inject a real post-terminal teardown failure.
struct FlakyVisibility {
    inner: Arc<dyn VisibilityStore>,
    fail: std::sync::atomic::AtomicBool,
}

impl FlakyVisibility {
    fn armed(inner: Arc<dyn VisibilityStore>) -> Self {
        Self {
            inner,
            fail: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn disarm(&self) {
        self.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl VisibilityStore for FlakyVisibility {
    async fn record_visibility(
        &self,
        record: aion_store::visibility::VisibilityRecord,
    ) -> Result<(), aion_store::StoreError> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(aion_store::StoreError::Backend(
                "forced visibility failure during deadline teardown".to_owned(),
            ));
        }
        self.inner.record_visibility(record).await
    }

    async fn list_workflows(
        &self,
        filter: aion_store::visibility::ListWorkflowsFilter,
    ) -> Result<Vec<aion_store::visibility::WorkflowSummary>, aion_store::StoreError> {
        self.inner.list_workflows(filter).await
    }

    async fn count_workflows(
        &self,
        filter: aion_store::visibility::ListWorkflowsFilter,
    ) -> Result<u64, aion_store::StoreError> {
        self.inner.count_workflows(filter).await
    }
}

/// A real post-append teardown failure (visibility unavailable) does NOT destroy
/// the retry anchors: the terminal is recorded, but the deadline stays live and
/// the run stays registered, and the handler PROPAGATES the failure. Once
/// visibility recovers, a re-fire resumes teardown to completion — proving the
/// deadline-stays-live-until-done ordering makes `ResumeTeardown` reachable after
/// a genuine failure.
#[tokio::test(flavor = "multi_thread")]
async fn teardown_failure_preserves_retry_anchors_then_resumes() -> TestResult {
    let backing = Arc::new(InMemoryStore::default());
    let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
    let visibility = Arc::new(FlakyVisibility::armed(backing));
    let visibility_store: Arc<dyn VisibilityStore> =
        Arc::clone(&visibility) as Arc<dyn VisibilityStore>;
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
    let deadline_id = crate::time::deadline_timer_id(&run_id)?;
    recorder
        .record_timer_started(chrono::Utc::now(), deadline_id.clone(), chrono::Utc::now())
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
    registry.insert((workflow_id.clone(), run_id.clone()), handle.clone())?;
    let handler = WorkflowDeadlineHandler::new(
        Arc::downgrade(&runtime),
        Arc::clone(&store),
        Arc::clone(&visibility_store),
        Arc::clone(&registry),
    );

    // First fire: the terminal records, then visibility fails and teardown is
    // propagated as an error.
    let first = handler
        .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
        .await;
    assert!(
        first.is_err(),
        "an incomplete teardown must be propagated, not swallowed"
    );
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(
        count_timed_out(&history),
        1,
        "the terminal is recorded once"
    );
    assert!(
        crate::time::outstanding_deadline_timer(&history, &run_id).is_some(),
        "the deadline stays live as the resume anchor: {history:#?}"
    );
    assert!(
        registry.get(&workflow_id, &run_id)?.is_some(),
        "the run stays registered as the second resume anchor"
    );

    // Visibility recovers; a re-fire resumes teardown to completion.
    visibility.disarm();
    handler
        .on_deadline_elapsed(workflow_id.clone(), run_id.clone())
        .await?;
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(
        count_timed_out(&history),
        1,
        "resuming records no second terminal: {history:#?}"
    );
    assert_eq!(
        crate::time::outstanding_deadline_timer(&history, &run_id),
        None,
        "the resumed teardown finally retires the deadline"
    );
    assert_eq!(
        registry.get(&workflow_id, &run_id)?,
        None,
        "the resumed teardown deregisters the run"
    );
    runtime.shutdown()?;
    Ok(())
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
async fn re_fire_after_recorded_timeout_resumes_teardown_without_a_second_terminal() -> TestResult {
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
