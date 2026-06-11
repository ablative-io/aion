//! Child-terminal watcher: the engine-side service that turns a child
//! workflow's terminal outcome into a recorded parent-side event plus a
//! pure wake marker, applying the signal router's record-before-wake
//! discipline to the parent's history.
//!
//! One watcher task is armed per `(parent pid, child workflow id)` await;
//! arming is idempotent and registered in the engine's [`ChildTaskRuntime`]
//! (which gates arming during shutdown and aborts-then-awaits every task at
//! epoch close). The loop treats the store as truth and each child-run
//! `CompletionNotifier` as a doorbell: on every iteration it re-reads the
//! child's durable history, follows the continue-as-new run chain to the
//! first non-CAN terminal, and only then records the parent's
//! `ChildWorkflowCompleted`/`ChildWorkflowFailed` — idempotently, under the
//! parent recorder lock, behind the same atomic parent-terminal guard the
//! signal router uses, retrying transient record failures with the engine's
//! backoff policy until the record lands or the parent run is terminal.
//! Marker delivery failure after the durable record is non-fatal: recovery
//! resolves the await from the recorded event.

use std::sync::Arc;

use aion_core::{Event, RunId, WorkflowError, WorkflowId};
use aion_store::EventStore;

use crate::durability::current_run_segment;
use crate::engine::delegated::run_has_terminal_history;
use crate::lifecycle::completion::terminal_outcome_from_history;
use crate::registry::{Registry, TerminalOutcome, WorkflowHandle};
use crate::runtime::nif_child_tasks::ChildTaskRuntime;
use crate::runtime::{RuntimeHandle, SignalDeliveryConfig};

/// Everything one watcher task needs; cheap clones of engine-owned seams.
#[derive(Clone)]
pub(super) struct ChildWatchContext {
    /// Durable event store (truth for child and parent histories).
    pub(super) store: Arc<dyn EventStore>,
    /// Active-execution registry used to resolve child run doorbells.
    pub(super) registry: Arc<Registry>,
    /// Runtime boundary used to deliver the wake marker.
    pub(super) runtime: Arc<RuntimeHandle>,
    /// Task registry and executor this watcher is armed in.
    pub(super) tasks: Arc<ChildTaskRuntime>,
    /// Builder-supplied backoff policy for registry-miss windows and
    /// transient record failures.
    pub(super) backoff: SignalDeliveryConfig,
    /// Awaiting parent's live handle (recorder, pid, run id).
    pub(super) parent: WorkflowHandle,
    /// Awaited child workflow identity.
    pub(super) child_workflow_id: WorkflowId,
}

/// Arm a child-terminal watcher for one `(parent pid, child id)` await.
///
/// Idempotent: a second arm while a watcher for the same key is running is
/// a no-op, and arming is refused once engine shutdown began. Returns
/// whether a new watcher task was spawned.
pub(super) fn arm_child_terminal_watch(context: ChildWatchContext) -> bool {
    let parent_pid = context.parent.pid();
    let child_id = context.child_workflow_id.clone();
    let tasks = Arc::clone(&context.tasks);
    tasks.arm_watch(parent_pid, child_id.clone(), async move {
        run_watch(&context).await;
        context.tasks.remove_watch(parent_pid, &child_id);
    })
}

/// One failed parent-side terminal record attempt.
#[derive(Debug)]
pub(super) enum RecordFailure {
    /// Transient store or recorder trouble; the watcher retries with the
    /// engine's backoff policy (F5) — a parked parent must never be
    /// abandoned over a transient append failure.
    Retryable(String),
    /// A logic violation that retrying can never repair.
    Invariant(String),
}

impl std::fmt::Display for RecordFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(reason) | Self::Invariant(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for RecordFailure {}

/// Drive one watcher to completion: store-truth loop, then record + wake.
///
/// The record is the durable handoff; without it the parent stays parked,
/// so a transient record failure is retried with backoff until it lands or
/// the parent run is observed terminal — never silently dropped (F5).
async fn run_watch(context: &ChildWatchContext) {
    let outcome = wait_for_child_terminal(context).await;
    let mut backoff = context.backoff.initial_backoff;
    let disposition = loop {
        match record_parent_child_terminal(&context.parent, &context.child_workflow_id, &outcome)
            .await
        {
            Ok(disposition) => break disposition,
            Err(RecordFailure::Retryable(reason)) => {
                tracing::warn!(
                    parent_workflow_id = %context.parent.workflow_id(),
                    parent_run_id = %context.parent.run_id(),
                    child_workflow_id = %context.child_workflow_id,
                    reason = %reason,
                    "child-terminal watcher record failed transiently; retrying with backoff"
                );
                sleep_backoff(&mut backoff, &context.backoff).await;
            }
            Err(RecordFailure::Invariant(reason)) => {
                tracing::error!(
                    parent_workflow_id = %context.parent.workflow_id(),
                    parent_run_id = %context.parent.run_id(),
                    child_workflow_id = %context.child_workflow_id,
                    reason = %reason,
                    "child-terminal watcher hit an unretryable record invariant violation"
                );
                return;
            }
        }
    };

    match disposition {
        RecordDisposition::ParentTerminal => {
            tracing::debug!(
                parent_workflow_id = %context.parent.workflow_id(),
                child_workflow_id = %context.child_workflow_id,
                "parent run is terminal; child-terminal watcher exits without recording"
            );
        }
        RecordDisposition::Recorded | RecordDisposition::AlreadyRecorded => {
            // Marker failure after the durable record is non-fatal: the
            // parent is gone or crashing, and recovery resolves the await
            // from the recorded event.
            if let Err(error) = context.runtime.deliver_child_terminal(context.parent.pid()) {
                tracing::warn!(
                    parent_workflow_id = %context.parent.workflow_id(),
                    parent_pid = context.parent.pid(),
                    child_workflow_id = %context.child_workflow_id,
                    error = %error,
                    "child terminal recorded durably but the wake marker could not be delivered"
                );
            }
        }
    }
}

/// Store-truth loop: poll the child's durable history, follow the
/// continue-as-new run chain, and park on the current run's completion
/// notifier between reads. Converges on every missed-edge race because the
/// store is re-read on each iteration.
async fn wait_for_child_terminal(context: &ChildWatchContext) -> TerminalOutcome {
    let mut backoff = context.backoff.initial_backoff;
    loop {
        let history = match context.store.read_history(&context.child_workflow_id).await {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    child_workflow_id = %context.child_workflow_id,
                    error = %error,
                    "child-terminal watcher could not read child history; backing off"
                );
                sleep_backoff(&mut backoff, &context.backoff).await;
                continue;
            }
        };

        let latest_run = latest_run_id(&history);
        let outcome = latest_run
            .as_ref()
            .and_then(|run_id| terminal_outcome_from_history(&history, run_id));
        match outcome {
            Some(
                terminal @ (TerminalOutcome::Completed(_)
                | TerminalOutcome::Failed(_)
                | TerminalOutcome::Cancelled(_)
                | TerminalOutcome::TimedOut(_)),
            ) => return terminal,
            // Continue-as-new is transparent to the awaiting parent: follow
            // the run chain to the first non-CAN terminal. The replacement
            // run's WorkflowStarted may not be appended yet; the next store
            // read converges.
            Some(TerminalOutcome::ContinuedAsNew { .. }) | None => {
                match current_run_handle(&context.registry, &context.child_workflow_id, latest_run)
                {
                    Some(handle) => {
                        let mut receiver = handle.completion().subscribe();
                        let published = receiver.borrow().clone();
                        if published.is_none() {
                            // Doorbell: the run's exit monitor publishes
                            // after recording. The wait is bounded — the
                            // store is the truth and the doorbell only an
                            // accelerator, so a missed ring (a notify path
                            // failure, an exit racing the subscription)
                            // degrades to the polling cadence instead of
                            // stranding the parked parent for the epoch.
                            let wait = doorbell_wait(&mut backoff, &context.backoff);
                            if tokio::time::timeout(wait, receiver.changed()).await.is_ok() {
                                // Rang (or closed): converge immediately and
                                // restart the polling ladder.
                                backoff = context.backoff.initial_backoff;
                            }
                            continue;
                        }
                        // The handle already published (mid-CAN window where
                        // the store has not caught up): poll with backoff.
                        sleep_backoff(&mut backoff, &context.backoff).await;
                    }
                    // Registry miss: recovery registration window, CAN
                    // replacement window, or a recorded-but-not-yet-swept
                    // child. Bounded backoff between store re-reads.
                    None => sleep_backoff(&mut backoff, &context.backoff).await,
                }
            }
        }
    }
}

/// Idempotency disposition of one parent-side terminal record attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecordDisposition {
    /// The terminal was recorded into the parent's current run segment.
    Recorded,
    /// The current run segment already holds a terminal for this child.
    AlreadyRecorded,
    /// The parent run is terminal; nothing was appended.
    ParentTerminal,
}

/// Record `ChildWorkflowCompleted`/`ChildWorkflowFailed` into the parent's
/// history, idempotently, under the parent recorder lock.
///
/// Mirrors the signal router's atomic terminal guard: the parent-terminal
/// check, the duplicate check, and the append happen under one lock
/// acquisition so a racing exit monitor can never interleave a terminal
/// between the check and the write.
///
/// # Errors
///
/// Returns [`RecordFailure::Retryable`] when the parent history cannot be
/// read or the recorder rejects the append, and
/// [`RecordFailure::Invariant`] for outcomes that are not recordable.
pub(super) async fn record_parent_child_terminal(
    parent: &WorkflowHandle,
    child_workflow_id: &WorkflowId,
    outcome: &TerminalOutcome,
) -> Result<RecordDisposition, RecordFailure> {
    let recorder = parent.recorder();
    let mut recorder = recorder.lock().await;
    let history = recorder.read_history().await.map_err(|error| {
        RecordFailure::Retryable(format!("parent history read failed: {error}"))
    })?;
    if run_has_terminal_history(&history, parent.run_id()) {
        return Ok(RecordDisposition::ParentTerminal);
    }
    let segment = current_run_segment(history, parent.run_id()).map_err(|error| {
        RecordFailure::Retryable(format!("parent run segment unavailable: {error}"))
    })?;
    if segment
        .iter()
        .any(|event| is_child_terminal_for(event, child_workflow_id))
    {
        return Ok(RecordDisposition::AlreadyRecorded);
    }

    let recorded_at = chrono::Utc::now();
    let retryable =
        |error: crate::durability::DurabilityError| RecordFailure::Retryable(error.to_string());
    match outcome {
        TerminalOutcome::Completed(result) => recorder
            .record_child_workflow_completed(recorded_at, child_workflow_id.clone(), result.clone())
            .await
            .map_err(retryable)?,
        TerminalOutcome::Failed(error) => recorder
            .record_child_workflow_failed(recorded_at, child_workflow_id.clone(), error.clone())
            .await
            .map_err(retryable)?,
        // Cancelled/TimedOut keep today's Failed mapping with message
        // prefixes; a distinct recorded taxonomy is out of scope (D-4).
        TerminalOutcome::Cancelled(reason) => recorder
            .record_child_workflow_failed(
                recorded_at,
                child_workflow_id.clone(),
                WorkflowError {
                    message: format!("cancelled:{reason}"),
                    details: None,
                },
            )
            .await
            .map_err(retryable)?,
        TerminalOutcome::TimedOut(timeout) => recorder
            .record_child_workflow_failed(
                recorded_at,
                child_workflow_id.clone(),
                WorkflowError {
                    message: format!("timed_out:{timeout}"),
                    details: None,
                },
            )
            .await
            .map_err(retryable)?,
        TerminalOutcome::ContinuedAsNew { .. } => {
            // The watch loop only surfaces real terminals; reaching here is
            // a logic error that must not silently corrupt parent history
            // and that no amount of retrying can repair.
            return Err(RecordFailure::Invariant(
                "continue-as-new is not a recordable child terminal; the run chain must be \
                 followed to a real terminal"
                    .to_owned(),
            ));
        }
    }
    Ok(RecordDisposition::Recorded)
}

/// Run id of the latest `WorkflowStarted` in a child history, if any.
pub(super) fn latest_run_id(history: &[Event]) -> Option<RunId> {
    history.iter().rev().find_map(|event| match event {
        Event::WorkflowStarted { run_id, .. } => Some(run_id.clone()),
        _ => None,
    })
}

/// Resolve the child's *current* run handle.
///
/// A continue-as-new chain leaves multiple `(workflow, run)` handles in the
/// registry for one workflow id; selecting by the latest recorded run id
/// (instead of an arbitrary `.find` over the bare workflow id) pins the
/// doorbell to the run whose terminal actually advances the chain. With no
/// recorded run yet (empty child history), any handle for the workflow id
/// would do — but the start path records `WorkflowStarted` before
/// registering, so an empty history means no handle either.
pub(super) fn current_run_handle(
    registry: &Registry,
    child_workflow_id: &WorkflowId,
    latest_run: Option<RunId>,
) -> Option<WorkflowHandle> {
    let run_id = latest_run?;
    match registry.get(child_workflow_id, &run_id) {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(
                child_workflow_id = %child_workflow_id,
                error = %error,
                "child-terminal watcher could not inspect the registry"
            );
            None
        }
    }
}

fn is_child_terminal_for(event: &Event, child_workflow_id: &WorkflowId) -> bool {
    matches!(
        event,
        Event::ChildWorkflowCompleted {
            child_workflow_id: recorded,
            ..
        } | Event::ChildWorkflowFailed {
            child_workflow_id: recorded,
            ..
        } if recorded == child_workflow_id
    )
}

async fn sleep_backoff(current: &mut std::time::Duration, policy: &SignalDeliveryConfig) {
    tokio::time::sleep(*current).await;
    let doubled = current.saturating_mul(2);
    *current = if doubled > policy.max_backoff {
        policy.max_backoff
    } else {
        doubled
    };
}

/// Bound for one doorbell wait, advancing the polling ladder.
///
/// Mirrors [`sleep_backoff`]'s progression but caps at the policy's
/// readiness horizon (`ready_timeout`, floored by `max_backoff`): the
/// doorbell stays the fast path, while a missed ring degrades to this
/// polling cadence against the store instead of an unbounded park.
fn doorbell_wait(
    current: &mut std::time::Duration,
    policy: &SignalDeliveryConfig,
) -> std::time::Duration {
    let cap = policy.ready_timeout.max(policy.max_backoff);
    let wait = *current;
    let doubled = wait.saturating_mul(2);
    *current = if doubled > cap { cap } else { doubled };
    wait
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use aion_core::{Event, Payload, RunId, WorkflowId, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::{
        ChildWatchContext, RecordDisposition, arm_child_terminal_watch, current_run_handle,
        latest_run_id, record_parent_child_terminal,
    };
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, TerminalOutcome, WorkflowHandle,
        WorkflowHandleParts,
    };
    use crate::runtime::nif_child_tasks::ChildTaskRuntime;
    use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    async fn started_handle(
        store: &Arc<dyn EventStore>,
        workflow_id: WorkflowId,
        run_id: RunId,
        pid: u64,
    ) -> Result<WorkflowHandle, Box<dyn std::error::Error>> {
        let head = store
            .read_history(&workflow_id)
            .await?
            .iter()
            .map(Event::seq)
            .max()
            .unwrap_or_default();
        let mut recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(store), head);
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                "parent".to_owned(),
                payload("input")?,
                run_id.clone(),
            )
            .await?;
        Ok(WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id,
            pid,
            workflow_type: "parent".to_owned(),
            loaded_version: ContentHash::from_bytes([3; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        }))
    }

    fn fast_backoff() -> SignalDeliveryConfig {
        SignalDeliveryConfig::new(
            Duration::ZERO,
            1,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
    }

    fn watch_context(
        store: Arc<dyn EventStore>,
        registry: Arc<Registry>,
        runtime: Arc<RuntimeHandle>,
        parent: WorkflowHandle,
        child_workflow_id: WorkflowId,
    ) -> Result<ChildWatchContext, Box<dyn std::error::Error>> {
        Ok(ChildWatchContext {
            store,
            registry,
            runtime,
            tasks: Arc::new(ChildTaskRuntime::new()?),
            backoff: fast_backoff(),
            parent,
            child_workflow_id,
        })
    }

    async fn child_terminal_count(
        store: &Arc<dyn EventStore>,
        parent: &WorkflowId,
        child: &WorkflowId,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        Ok(store
            .read_history(parent)
            .await?
            .iter()
            .filter(|event| super::is_child_terminal_for(event, child))
            .count())
    }

    #[tokio::test]
    async fn arming_is_idempotent_per_parent_and_child() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 21).await?;
        let child = WorkflowId::new_v4();
        let mut context = watch_context(
            Arc::clone(&store),
            registry,
            Arc::clone(&runtime),
            parent,
            child.clone(),
        )?;
        let tasks = Arc::clone(&context.tasks);

        let first = arm_child_terminal_watch(context.clone());
        let second = arm_child_terminal_watch(context.clone());

        assert!(first, "first arm must spawn a watcher");
        assert!(!second, "second arm for the same key must be a no-op");
        assert_eq!(tasks.armed_watch_count(), 1);

        // A different child id under the same parent is its own watcher.
        context.child_workflow_id = WorkflowId::new_v4();
        assert!(arm_child_terminal_watch(context));
        assert_eq!(tasks.armed_watch_count(), 2);

        tasks.shutdown();
        assert_eq!(tasks.armed_watch_count(), 0);
        runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_aborts_only_the_exited_parents_watchers() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let parent_a = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 31).await?;
        let parent_b = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 32).await?;
        let tasks = Arc::new(ChildTaskRuntime::new()?);
        for parent in [parent_a, parent_b] {
            let context = ChildWatchContext {
                store: Arc::clone(&store),
                registry: Arc::clone(&registry),
                runtime: Arc::clone(&runtime),
                tasks: Arc::clone(&tasks),
                backoff: fast_backoff(),
                parent,
                child_workflow_id: WorkflowId::new_v4(),
            };
            assert!(arm_child_terminal_watch(context));
        }
        assert_eq!(tasks.armed_watch_count(), 2);

        tasks.abort_watches_for_parent(31);

        assert_eq!(tasks.armed_watch_count(), 1);
        tasks.shutdown();
        runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn record_is_terminal_guarded_and_duplicate_suppressed() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 41).await?;
        let child = WorkflowId::new_v4();
        let outcome = TerminalOutcome::Completed(payload("child-result")?);

        let first = record_parent_child_terminal(&parent, &child, &outcome).await?;
        let second = record_parent_child_terminal(&parent, &child, &outcome).await?;

        assert_eq!(first, RecordDisposition::Recorded);
        assert_eq!(second, RecordDisposition::AlreadyRecorded);
        assert_eq!(
            child_terminal_count(&store, parent.workflow_id(), &child).await?,
            1,
            "duplicate record attempts must not append"
        );

        // A second child id is independent of the first's dedup.
        let other_child = WorkflowId::new_v4();
        assert_eq!(
            record_parent_child_terminal(&parent, &other_child, &outcome).await?,
            RecordDisposition::Recorded
        );

        // Parent terminal: nothing may be appended after the terminal event.
        {
            let recorder = parent.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), payload("done")?)
                .await?;
        }
        let history_len = store.read_history(parent.workflow_id()).await?.len();
        assert_eq!(
            record_parent_child_terminal(&parent, &WorkflowId::new_v4(), &outcome).await?,
            RecordDisposition::ParentTerminal
        );
        assert_eq!(
            store.read_history(parent.workflow_id()).await?.len(),
            history_len,
            "terminal-guarded record must append nothing"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_and_timed_out_map_to_failed_with_prefixes() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 42).await?;
        let cancelled_child = WorkflowId::new_v4();
        let timed_out_child = WorkflowId::new_v4();

        record_parent_child_terminal(
            &parent,
            &cancelled_child,
            &TerminalOutcome::Cancelled("operator".to_owned()),
        )
        .await?;
        record_parent_child_terminal(
            &parent,
            &timed_out_child,
            &TerminalOutcome::TimedOut("30s".to_owned()),
        )
        .await?;

        let history = store.read_history(parent.workflow_id()).await?;
        let messages: Vec<_> = history
            .iter()
            .filter_map(|event| match event {
                Event::ChildWorkflowFailed { error, .. } => Some(error.message.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages,
            vec!["cancelled:operator".to_owned(), "timed_out:30s".to_owned()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn continue_as_new_outcome_is_refused_as_a_recordable_terminal() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 43).await?;
        let before = store.read_history(parent.workflow_id()).await?;

        let error = record_parent_child_terminal(
            &parent,
            &WorkflowId::new_v4(),
            &TerminalOutcome::ContinuedAsNew {
                input: payload("next")?,
                workflow_type: None,
                parent_run_id: RunId::new_v4(),
            },
        )
        .await
        .err()
        .ok_or("continue-as-new was accepted as a recordable terminal")?;

        assert!(
            matches!(&error, super::RecordFailure::Invariant(reason)
                if reason.contains("not a recordable child terminal")),
            "expected an invariant refusal, got: {error}"
        );
        assert_eq!(store.read_history(parent.workflow_id()).await?, before);
        Ok(())
    }

    #[tokio::test]
    async fn watcher_follows_continue_as_new_chain_to_final_terminal() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 51).await?;
        let child_id = WorkflowId::new_v4();
        let first_run = RunId::new_v4();
        let second_run = RunId::new_v4();

        // Child run 1 starts and continues-as-new (registry holds run 1).
        let first_handle = started_handle(&store, child_id.clone(), first_run.clone(), 52).await?;
        registry.insert((child_id.clone(), first_run.clone()), first_handle.clone())?;
        {
            let recorder = first_handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_continued_as_new(
                    chrono::Utc::now(),
                    payload("next")?,
                    None,
                    first_run.clone(),
                )
                .await?;
        }
        first_handle
            .completion()
            .notify(TerminalOutcome::ContinuedAsNew {
                input: payload("next")?,
                workflow_type: None,
                parent_run_id: first_run,
            });

        let context = watch_context(
            Arc::clone(&store),
            Arc::clone(&registry),
            Arc::clone(&runtime),
            parent.clone(),
            child_id.clone(),
        )?;
        assert!(arm_child_terminal_watch(context.clone()));

        // The watcher must wait through the CAN window (replacement not yet
        // registered) without recording anything.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            child_terminal_count(&store, parent.workflow_id(), &child_id).await?,
            0,
            "continue-as-new must not satisfy the await"
        );

        // The replacement run starts, registers, then completes.
        let second_handle =
            started_handle(&store, child_id.clone(), second_run.clone(), 53).await?;
        registry.insert((child_id.clone(), second_run), second_handle.clone())?;
        let final_result = payload("final-result")?;
        {
            let recorder = second_handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), final_result.clone())
                .await?;
        }
        second_handle
            .completion()
            .notify(TerminalOutcome::Completed(final_result.clone()));

        // The watcher records the final run's result against the stable
        // child workflow id. (Marker delivery to the fake pid fails and is
        // non-fatal by contract.)
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let history = store.read_history(parent.workflow_id()).await?;
            if let Some(Event::ChildWorkflowCompleted {
                child_workflow_id,
                result,
                ..
            }) = history
                .iter()
                .find(|event| matches!(event, Event::ChildWorkflowCompleted { .. }))
            {
                assert_eq!(child_workflow_id, &child_id);
                assert_eq!(result, &final_result);
                break;
            }
            if std::time::Instant::now() > deadline {
                return Err(format!("watcher never recorded the terminal: {history:#?}").into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The dedup entry is removed once the watcher finishes.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while context.tasks.armed_watch_count() != 0 {
            if std::time::Instant::now() > deadline {
                return Err("watcher entry was not removed after completion".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        runtime.shutdown()?;
        Ok(())
    }

    /// The store is the truth and the completion doorbell only an
    /// accelerator: a child terminal recorded without its notify ever firing
    /// (a notify path failure, an exit racing the subscription) must still
    /// be observed through the bounded doorbell wait. Before the fix the
    /// watcher parked on `receiver.changed()` forever and the awaiting
    /// parent was stranded for the epoch — the ~1/350 restart-family flake.
    #[tokio::test]
    async fn missed_doorbell_degrades_to_store_polling_not_a_permanent_park() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 71).await?;
        let child_id = WorkflowId::new_v4();
        let child_run = RunId::new_v4();
        let child_handle = started_handle(&store, child_id.clone(), child_run.clone(), 72).await?;
        registry.insert((child_id.clone(), child_run), child_handle.clone())?;

        // The watcher arms against a live, non-terminal child and parks on
        // the doorbell.
        let context = watch_context(
            Arc::clone(&store),
            registry,
            Arc::clone(&runtime),
            parent.clone(),
            child_id.clone(),
        )?;
        let tasks = Arc::clone(&context.tasks);
        assert!(arm_child_terminal_watch(context));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            child_terminal_count(&store, parent.workflow_id(), &child_id).await?,
            0
        );

        // The child terminal lands durably but the doorbell NEVER rings
        // (no `completion().notify(..)` call).
        {
            let recorder = child_handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), payload("muted-doorbell")?)
                .await?;
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while child_terminal_count(&store, parent.workflow_id(), &child_id).await? != 1
            || tasks.armed_watch_count() != 0
        {
            if std::time::Instant::now() > deadline {
                return Err("watcher stayed parked on a muted doorbell despite store truth".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn registry_miss_backs_off_until_the_child_appears() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 61).await?;
        let child_id = WorkflowId::new_v4();

        // Arm before the child exists anywhere (the recovery-sweep window:
        // recorded ChildWorkflowStarted, child history empty, no handle).
        let context = watch_context(
            Arc::clone(&store),
            Arc::clone(&registry),
            Arc::clone(&runtime),
            parent.clone(),
            child_id.clone(),
        )?;
        assert!(arm_child_terminal_watch(context));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            child_terminal_count(&store, parent.workflow_id(), &child_id).await?,
            0
        );

        // The child appears (history + registry) and completes; the parked
        // backoff loop converges on the store truth.
        let child_run = RunId::new_v4();
        let child_handle = started_handle(&store, child_id.clone(), child_run.clone(), 62).await?;
        registry.insert((child_id.clone(), child_run), child_handle.clone())?;
        {
            let recorder = child_handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), payload("late-result")?)
                .await?;
        }
        child_handle
            .completion()
            .notify(TerminalOutcome::Completed(payload("late-result")?));

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while child_terminal_count(&store, parent.workflow_id(), &child_id).await? != 1 {
            if std::time::Instant::now() > deadline {
                return Err("watcher never converged after the registry miss window".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn marker_failure_after_durable_record_is_non_fatal() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        // Parent pid 9_999 was never spawned: deliver_child_terminal fails.
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 9_999).await?;
        let child_id = WorkflowId::new_v4();
        let child_run = RunId::new_v4();
        let child_handle = started_handle(&store, child_id.clone(), child_run.clone(), 63).await?;
        registry.insert((child_id.clone(), child_run), child_handle.clone())?;
        {
            let recorder = child_handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), payload("result")?)
                .await?;
        }
        child_handle
            .completion()
            .notify(TerminalOutcome::Completed(payload("result")?));

        let context = watch_context(
            Arc::clone(&store),
            registry,
            Arc::clone(&runtime),
            parent.clone(),
            child_id.clone(),
        )?;
        let tasks = Arc::clone(&context.tasks);
        assert!(arm_child_terminal_watch(context));

        // The record lands durably and the watcher exits cleanly despite the
        // undeliverable marker.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while child_terminal_count(&store, parent.workflow_id(), &child_id).await? != 1
            || tasks.armed_watch_count() != 0
        {
            if std::time::Instant::now() > deadline {
                return Err("watcher did not record and exit after marker failure".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn latest_run_id_reads_the_newest_started_run() -> TestResult {
        let workflow_id = WorkflowId::new_v4();
        let first = RunId::new_v4();
        let second = RunId::new_v4();
        let envelope = |seq| aion_core::EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        };
        let history = vec![
            Event::WorkflowStarted {
                envelope: envelope(1),
                workflow_type: "child".to_owned(),
                input: payload("first")?,
                run_id: first.clone(),
                parent_run_id: None,
            },
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2),
                input: payload("next")?,
                workflow_type: None,
                parent_run_id: first.clone(),
            },
            Event::WorkflowStarted {
                envelope: envelope(3),
                workflow_type: "child".to_owned(),
                input: payload("next")?,
                run_id: second.clone(),
                parent_run_id: Some(first),
            },
        ];

        assert_eq!(latest_run_id(&history), Some(second));
        assert_eq!(latest_run_id(&[]), None);
        Ok(())
    }

    /// Delegating store whose `append` fails while a failure budget remains.
    struct FlakyAppendStore {
        inner: InMemoryStore,
        remaining_failures: std::sync::atomic::AtomicU32,
    }

    impl FlakyAppendStore {
        fn new() -> Self {
            Self {
                inner: InMemoryStore::default(),
                remaining_failures: std::sync::atomic::AtomicU32::new(0),
            }
        }

        fn fail_next_appends(&self, count: u32) {
            self.remaining_failures
                .store(count, std::sync::atomic::Ordering::Release);
        }
    }

    #[async_trait::async_trait]
    impl aion_store::ReadableEventStore for FlakyAppendStore {
        async fn read_history(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<Event>, aion_store::StoreError> {
            self.inner.read_history(workflow_id).await
        }

        async fn read_history_from(
            &self,
            workflow_id: &WorkflowId,
            from_seq: u64,
        ) -> Result<Vec<Event>, aion_store::StoreError> {
            self.inner.read_history_from(workflow_id, from_seq).await
        }

        async fn read_run_chain(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<aion_store::RunSummary>, aion_store::StoreError> {
            self.inner.read_run_chain(workflow_id).await
        }

        async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, aion_store::StoreError> {
            self.inner.list_workflow_ids().await
        }

        async fn list_active(&self) -> Result<Vec<WorkflowId>, aion_store::StoreError> {
            self.inner.list_active().await
        }

        async fn query(
            &self,
            filter: &aion_core::WorkflowFilter,
        ) -> Result<Vec<aion_core::WorkflowSummary>, aion_store::StoreError> {
            self.inner.query(filter).await
        }

        async fn schedule_timer(
            &self,
            workflow_id: &WorkflowId,
            timer_id: &aion_core::TimerId,
            fire_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), aion_store::StoreError> {
            self.inner
                .schedule_timer(workflow_id, timer_id, fire_at)
                .await
        }

        async fn expired_timers(
            &self,
            as_of: chrono::DateTime<chrono::Utc>,
        ) -> Result<Vec<aion_store::TimerEntry>, aion_store::StoreError> {
            self.inner.expired_timers(as_of).await
        }
    }

    #[async_trait::async_trait]
    impl aion_store::WritableEventStore for FlakyAppendStore {
        async fn append(
            &self,
            token: aion_store::WriteToken,
            workflow_id: &WorkflowId,
            events: &[Event],
            expected_seq: u64,
        ) -> Result<(), aion_store::StoreError> {
            let failing = self
                .remaining_failures
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |current| current.checked_sub(1),
                )
                .is_ok();
            if failing {
                return Err(aion_store::StoreError::Backend(
                    "transient append failure injected by FlakyAppendStore".to_owned(),
                ));
            }
            self.inner
                .append(token, workflow_id, events, expected_seq)
                .await
        }
    }

    /// F5: a transient record failure must be retried with backoff until the
    /// record lands — the parked parent is never abandoned for the epoch.
    /// Before the fix the watcher logged once and exited, leaving the parent
    /// parked forever; this test then failed its convergence deadline.
    #[tokio::test]
    async fn transient_record_failure_is_retried_until_the_terminal_lands() -> TestResult {
        let flaky = Arc::new(FlakyAppendStore::new());
        let store: Arc<dyn EventStore> = Arc::clone(&flaky) as Arc<dyn EventStore>;
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let parent = started_handle(&store, WorkflowId::new_v4(), RunId::new_v4(), 81).await?;
        let child_id = WorkflowId::new_v4();
        let child_run = RunId::new_v4();
        let child_handle = started_handle(&store, child_id.clone(), child_run.clone(), 82).await?;
        registry.insert((child_id.clone(), child_run), child_handle.clone())?;
        {
            let recorder = child_handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), payload("flaky-result")?)
                .await?;
        }
        child_handle
            .completion()
            .notify(TerminalOutcome::Completed(payload("flaky-result")?));

        // Every store write from here fails three times before succeeding:
        // the watcher's parent-side record must survive all of them.
        flaky.fail_next_appends(3);
        let context = watch_context(
            Arc::clone(&store),
            registry,
            Arc::clone(&runtime),
            parent.clone(),
            child_id.clone(),
        )?;
        let tasks = Arc::clone(&context.tasks);
        assert!(arm_child_terminal_watch(context));

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while child_terminal_count(&store, parent.workflow_id(), &child_id).await? != 1
            || tasks.armed_watch_count() != 0
        {
            if std::time::Instant::now() > deadline {
                return Err(
                    "watcher abandoned the record after transient failures (F5 regression)".into(),
                );
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            child_terminal_count(&store, parent.workflow_id(), &child_id).await?,
            1,
            "the retried record must land exactly once"
        );
        runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn current_run_handle_selects_the_latest_run_not_an_arbitrary_one() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Registry::default();
        let child_id = WorkflowId::new_v4();
        let old_run = RunId::new_v4();
        let new_run = RunId::new_v4();
        let old_handle = started_handle(&store, child_id.clone(), old_run.clone(), 71).await?;
        let new_handle = started_handle(&store, child_id.clone(), new_run.clone(), 72).await?;
        registry.insert((child_id.clone(), old_run), old_handle)?;
        registry.insert((child_id.clone(), new_run.clone()), new_handle)?;

        let resolved = current_run_handle(&registry, &child_id, Some(new_run.clone()))
            .ok_or("latest run handle was not resolved")?;

        assert_eq!(resolved.run_id(), &new_run);
        assert_eq!(resolved.pid(), 72);
        assert!(current_run_handle(&registry, &child_id, None).is_none());
        Ok(())
    }
}
