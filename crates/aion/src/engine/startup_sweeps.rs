//! Crash-window repair sweeps run by startup recovery: recorded-but-never-
//! spawned children (#56) and continue-as-new successors whose start was
//! lost between the terminal record and the successor's `WorkflowStarted`.

use std::sync::Arc;

use aion_core::{Event, RunId, WorkflowFilter, WorkflowStatus};
use chrono::Utc;

use crate::{
    EngineError,
    durability::current_run_segment,
    lifecycle::start::{StartWorkflowContext, StartWorkflowOptions, start_workflow_with_options},
};

use super::startup::StartupRecoveryContext;

/// Start every recorded-but-never-spawned child of a recovered parent (#56).
///
/// Record-then-spawn means a crash between the parent's durable
/// `ChildWorkflowStarted` and the child's actual start leaves a child with
/// a recorded identity but no history. The sweep repairs that window: for
/// each `ChildWorkflowStarted` in the recovered run segment without a
/// parent-side terminal, an *empty* child history means the child never
/// started — start it now under the recorded id, type, and input.
/// Idempotent: a non-empty child history means the child exists (its own
/// `list_active` recovery owns its process), and the parent's replayed
/// spawn resolves from the recorded event, so no path starts a duplicate.
/// The sweep also covers fire-and-forget children, which no await would
/// ever lazily repair.
pub(super) async fn sweep_recorded_children(
    context: &StartupRecoveryContext,
    parent_workflow_id: &aion_core::WorkflowId,
    parent_run_id: &RunId,
    parent_history: &[Event],
) -> Result<(), EngineError> {
    let segment = current_run_segment(parent_history.to_vec(), parent_run_id)?;
    for event in &segment {
        let Event::ChildWorkflowStarted {
            child_workflow_id,
            workflow_type,
            input,
            package_version,
            ..
        } = event
        else {
            continue;
        };
        let has_parent_side_terminal = segment.iter().any(|candidate| {
            matches!(
                candidate,
                Event::ChildWorkflowCompleted { child_workflow_id: recorded, .. }
                | Event::ChildWorkflowFailed { child_workflow_id: recorded, .. }
                    if recorded == child_workflow_id
            )
        });
        if has_parent_side_terminal {
            continue;
        }
        let child_history = context
            .store
            .as_ref()
            .read_history(child_workflow_id)
            .await?;
        if !child_history.is_empty() {
            continue;
        }
        tracing::info!(
            parent_workflow_id = %parent_workflow_id,
            child_workflow_id = %child_workflow_id,
            workflow_type = %workflow_type,
            "starting recorded-but-never-spawned child found by the recovery sweep"
        );
        start_workflow_with_options(
            StartWorkflowContext {
                store: Arc::clone(&context.store),
                visibility_store: Arc::clone(&context.visibility_store),
                catalog: Arc::clone(&context.catalog),
                runtime: Arc::clone(&context.runtime),
                supervision: Arc::clone(&context.supervision),
                registry: Arc::clone(&context.registry),
                signal_handoff: None,
                search_attribute_schema: Arc::clone(&context.search_attribute_schema),
                monitor_tokio_handle: tokio::runtime::Handle::current(),
            },
            workflow_type,
            input.clone(),
            StartWorkflowOptions {
                workflow_id: Some(child_workflow_id.clone()),
                // The crash path resolves exactly the version the parent
                // recorded at spawn-record time (D1), never a fresh "latest".
                loaded_version: Some(crate::loader::parse_package_version(
                    workflow_type,
                    package_version,
                )?),
                ..StartWorkflowOptions::default()
            },
        )
        .await?;
    }
    Ok(())
}

/// Start the recorded-but-never-started successor run for every workflow
/// whose latest run continued as new — the continue-as-new dual of
/// [`sweep_recorded_children`].
///
/// The successor run is normally started by the exiting run's monitor
/// (`start_continuation_replacement`); a crash after the
/// `WorkflowContinuedAsNew` record but before the successor's
/// `WorkflowStarted` leaves the whole run chain wedged: the history projects
/// the *terminal* `ContinuedAsNew` status, so `list_active` never surfaces
/// the workflow and no recovery path restarts it — and a parent awaiting the
/// child backs off in its watcher forever. This sweep enumerates exactly
/// those histories by status projection and starts the successor under the
/// recorded identity, input, and run chain.
///
/// Idempotent: a started successor appends a `WorkflowStarted` that flips
/// the projection back to `Running`, so a repaired workflow never matches
/// the enumeration again, and the in-history `parent_run_id` guard mirrors
/// `start_continuation_replacement`'s own duplicate check.
pub(super) async fn sweep_continued_as_new_replacements(
    context: &StartupRecoveryContext,
) -> Result<(), EngineError> {
    let stranded = context
        .store
        .as_ref()
        .query(&WorkflowFilter {
            status: Some(WorkflowStatus::ContinuedAsNew),
            ..WorkflowFilter::default()
        })
        .await?;
    for summary in stranded {
        let workflow_id = summary.workflow_id;
        let history = context.store.as_ref().read_history(&workflow_id).await?;
        // The most recent rotation is the one that lost its successor; any
        // earlier rotation already has a later `WorkflowStarted`.
        let Some((input, type_override, continued_run_id)) =
            history.iter().rev().find_map(|event| match event {
                Event::WorkflowContinuedAsNew {
                    input,
                    workflow_type,
                    parent_run_id,
                    ..
                } => Some((input.clone(), workflow_type.clone(), parent_run_id.clone())),
                _ => None,
            })
        else {
            // The projection said continue-as-new but the event is gone —
            // a racing append between the query and the read. Nothing to
            // repair against this snapshot.
            continue;
        };
        let already_started = history.iter().any(|event| {
            matches!(
                event,
                Event::WorkflowStarted {
                    parent_run_id: Some(existing),
                    ..
                } if existing == &continued_run_id
            )
        });
        if already_started {
            continue;
        }
        let replacement_type = match type_override {
            Some(workflow_type) => workflow_type,
            None => continued_run_workflow_type(&workflow_id, &history, &continued_run_id)?,
        };
        tracing::info!(
            workflow_id = %workflow_id,
            continued_run_id = %continued_run_id,
            workflow_type = %replacement_type,
            "starting continue-as-new successor run found by the recovery sweep"
        );
        let started = start_workflow_with_options(
            StartWorkflowContext {
                store: Arc::clone(&context.store),
                visibility_store: Arc::clone(&context.visibility_store),
                catalog: Arc::clone(&context.catalog),
                runtime: Arc::clone(&context.runtime),
                supervision: Arc::clone(&context.supervision),
                registry: Arc::clone(&context.registry),
                signal_handoff: None,
                search_attribute_schema: Arc::clone(&context.search_attribute_schema),
                monitor_tokio_handle: tokio::runtime::Handle::current(),
            },
            &replacement_type,
            input,
            StartWorkflowOptions {
                workflow_id: Some(workflow_id.clone()),
                parent_run_id: Some(continued_run_id.clone()),
                // Recorded attributes carry into the replacement run's
                // projection, exactly as in the monitor's replacement start.
                ..StartWorkflowOptions::default()
            },
        )
        .await;
        if let Err(error) = started {
            // The sweep races a recovered workflow's exit monitor, which
            // starts the same successor through
            // `start_continuation_replacement` with no per-id serialization.
            // The loser's recorder append surfaces a `SequenceConflict` (or
            // a downstream start failure) — benign exactly when the winner's
            // successor `WorkflowStarted` is now durable. Re-read history and
            // treat that as success; everything else still fails the build.
            if successor_started(context, &workflow_id, &continued_run_id).await? {
                tracing::info!(
                    workflow_id = %workflow_id,
                    continued_run_id = %continued_run_id,
                    error = %error,
                    "continue-as-new sweep lost the start race to the exit monitor; \
                     successor run is durable"
                );
                continue;
            }
            return Err(error);
        }
    }
    Ok(())
}

/// Retire every declared-timeout deadline left uncancelled behind a NON-timeout
/// terminal — the crash-window repair for the two-write terminal transition.
///
/// A terminal writer records its terminal and then, as a SEPARATE durable write,
/// cancels the run's recorded deadline. A crash (or a transient store failure)
/// between the two leaves a run that is durably `Completed`/`Failed`/`Cancelled`
/// or continued-as-new while its `deadline:{run}` timer is still live —
/// permanently violating D5 and letting whole-history recovery keep re-arming it.
/// This sweep drives that condition to closure at startup and at shard adoption,
/// in addition to the process-exit re-entry (`handle_process_exit`) and the
/// deadline-fire repair (`WorkflowDeadlineHandler`), so no history can hold a
/// terminal + an uncancelled recorded deadline across a restart or a failover.
///
/// The candidate set is the durable timer rows (bounded by armed timers, not all
/// workflows), filtered to deadline ids. A `WorkflowTimedOut` terminal is
/// deliberately SKIPPED: its deadline is owned by `WorkflowDeadlineHandler`
/// teardown, and `recover_due` re-fires it to drive `ResumeTeardown`. A run with
/// no terminal yet keeps its live deadline. Idempotent.
///
/// Writer discipline (honest bound). Ordering removes every RECOVERY-path writer
/// from this sweep's window: on cold boot it runs BEFORE repopulation and
/// continue-as-new successor start, and on adoption BEFORE the acquired shards'
/// workflows are repopulated — so no recovery-created recorder exists for a
/// candidate. It is NOT, however, exclusive against public post-fence actors:
/// adoption publishes shard ownership before recovery, so `Engine::reopen_workflow`
/// (and other public writers) can create a recorder for an acquired workflow
/// concurrently with this sweep. The sweep is therefore an OPTIMISTIC writer whose
/// serialization point is the store's per-workflow sequence check; a lost append
/// is arbitrated by the COMPLETE repair predicate (see
/// [`retire_orphaned_terminal_deadline_independent`]), which recognizes a
/// concurrent `WorkflowReopened` as making the repair inapplicable rather than an
/// error.
///
/// [`SweepScope`] selects what a live registered handle at candidate time means:
/// on cold boot it is an ordering-invariant breach (typed error); on adoption it
/// is an already-owned resident workflow that is out of scope (skipped — its
/// deadline lifecycle is this engine's normal-operation concern, and the acquired
/// workflows it is here to repair have no local handle yet).
///
/// Future note (out of scope for this lane): the adoption sweep enumerates the
/// full owned timer scope before repopulating the acquired workflows, which
/// lengthens the already-published/pre-recovery window in proportion to all owned
/// timer rows. A workflow-scoped writer-generation gate shared with reopen/start
/// would tighten this, but the optimistic arbitration above is the intended shape
/// here.
pub(super) async fn sweep_uncancelled_terminal_deadlines(
    context: &StartupRecoveryContext,
    scope: SweepScope,
) -> Result<(), EngineError> {
    let horizon = Utc::now()
        .checked_add_signed(chrono::Duration::days(3_652_500))
        .unwrap_or_else(Utc::now);
    let rows = context.store.as_ref().expired_timers(horizon).await?;
    for entry in rows {
        let Some(run_id) = crate::time::deadline_run_id(&entry.timer_id) else {
            continue;
        };
        repair_orphaned_terminal_deadline(context, &entry.workflow_id, &run_id, scope).await?;
    }
    Ok(())
}

/// Whether this sweep runs at cold boot (full scope, no local recorders exist
/// yet) or at live shard adoption (scoped implicitly to the not-yet-repopulated
/// acquired workflows — those without a live registered handle).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SweepScope {
    /// Cold boot before repopulation: NO candidate may have a live handle.
    ColdBoot,
    /// Live adoption before repopulating the acquired shards' workflows: an
    /// already-owned resident workflow legitimately has a live handle and is out
    /// of scope.
    Adoption,
}

/// Repair one candidate's uncancelled non-timeout-terminal deadline.
///
/// A live registered handle means a live local recorder exists for the workflow.
/// On cold boot that is an ordering-invariant breach (the sweep must precede
/// repopulation), surfaced as a typed [`EngineError`] rather than an append
/// around the live recorder or a silent skip. On adoption it is an out-of-scope
/// already-owned workflow, skipped. Otherwise no recovery-path recorder exists, so
/// the retirement uses an independent recorder as an OPTIMISTIC writer, with
/// `SequenceConflict` reconciliation (see
/// [`retire_orphaned_terminal_deadline_independent`]) arbitrating against a
/// concurrent public writer such as `reopen`.
async fn repair_orphaned_terminal_deadline(
    context: &StartupRecoveryContext,
    workflow_id: &aion_core::WorkflowId,
    run_id: &RunId,
    scope: SweepScope,
) -> Result<(), EngineError> {
    // Resolve the workflow's live run through the live index (never `list().find`,
    // which can select a superseded run's stale handle).
    if context.registry.live_run_pid(workflow_id)?.is_some() {
        return match scope {
            SweepScope::ColdBoot => Err(EngineError::Runtime {
                reason: format!(
                    "terminal-deadline sweep found a live registered handle for workflow {workflow_id} on cold boot, which must run before repopulation: the sweep ordering invariant is broken"
                ),
            }),
            SweepScope::Adoption => Ok(()),
        };
    }
    retire_orphaned_terminal_deadline_independent(context, workflow_id, run_id).await
}

/// Whether `run_id`'s history shows a NON-timeout terminal with a still-outstanding
/// deadline — the crash-window condition this sweep repairs.
fn is_orphaned_terminal_deadline(history: &[Event], run_id: &RunId) -> bool {
    if crate::time::outstanding_deadline_timer(history, run_id).is_none() {
        return false;
    }
    matches!(
        crate::lifecycle::completion::terminal_outcome_from_history(history, run_id),
        Some(terminal) if !matches!(terminal, crate::registry::TerminalOutcome::TimedOut(_))
    )
}

/// Retire the deadline through an independent recorder as an OPTIMISTIC writer:
/// the store's per-workflow sequence check is the serialization point. On a
/// `SequenceConflict` (a cross-actor writer won the head), re-read and re-evaluate
/// the COMPLETE repair predicate; if the run is no longer an orphaned terminal
/// deadline the repair became inapplicable — success, nothing to do — otherwise
/// surface the typed error, never swallow it.
async fn retire_orphaned_terminal_deadline_independent(
    context: &StartupRecoveryContext,
    workflow_id: &aion_core::WorkflowId,
    run_id: &RunId,
) -> Result<(), EngineError> {
    let history = context.store.as_ref().read_history(workflow_id).await?;
    if !is_orphaned_terminal_deadline(&history, run_id) {
        return Ok(());
    }
    let head = history.iter().map(Event::seq).max().unwrap_or_default();
    let mut recorder = crate::durability::Recorder::resume_at(
        workflow_id.clone(),
        Arc::clone(&context.store),
        head,
    );
    tracing::info!(
        %workflow_id,
        %run_id,
        "retiring an orphaned non-timeout terminal deadline via an independent recorder (startup repair)"
    );
    match crate::time::retire_run_deadline(&mut recorder, &history, run_id).await {
        Ok(()) => Ok(()),
        Err(error) => {
            if matches!(
                error,
                crate::durability::DurabilityError::Store(
                    aion_store::StoreError::SequenceConflict { .. }
                )
            ) {
                // A cross-actor writer advanced the head between our snapshot and
                // our append. Re-read and re-evaluate the COMPLETE repair
                // predicate: if the run is no longer an orphaned terminal deadline,
                // the repair became inapplicable and there is nothing to do —
                // success, not error. This covers a competing terminal writer that
                // retired the deadline AND a concurrent public `WorkflowReopened`
                // (a valid post-fence actor during adoption): reopen clears the
                // terminal predicate while deliberately keeping the failed run's
                // deadline live, so the run ceased to be a crash-window orphan and
                // must NOT be retired here and must NOT abort adoption.
                //
                // The sweep-WINS direction needs no code here: a normally-failed
                // run has its deadline retired by the terminal writer BEFORE any
                // reopen can occur, so the sweep retiring the crash-window orphan
                // first makes the crash case converge to the normal case. A
                // concurrent reopen that instead loses this append fails with a
                // retryable `SequenceConflict`, and its own retry then reads the
                // repaired history — a benign transient of a rare coincidence
                // (reopen-during-adoption of a crash-window run).
                let history = context.store.as_ref().read_history(workflow_id).await?;
                if !is_orphaned_terminal_deadline(&history, run_id) {
                    return Ok(());
                }
            }
            Err(error.into())
        }
    }
}

/// Whether a successor `WorkflowStarted` continuing `continued_run_id` is
/// durable for `workflow_id`.
async fn successor_started(
    context: &StartupRecoveryContext,
    workflow_id: &aion_core::WorkflowId,
    continued_run_id: &RunId,
) -> Result<bool, EngineError> {
    let history = context.store.as_ref().read_history(workflow_id).await?;
    Ok(history.iter().any(|event| {
        matches!(
            event,
            Event::WorkflowStarted {
                parent_run_id: Some(existing),
                ..
            } if existing == continued_run_id
        )
    }))
}

/// Workflow type of the run that recorded the continue-as-new terminal.
///
/// The replacement inherits it when the rotation carried no type override —
/// the startup-time equivalent of the exit monitor's `handle.workflow_type()`
/// fallback.
fn continued_run_workflow_type(
    workflow_id: &aion_core::WorkflowId,
    history: &[Event],
    continued_run_id: &RunId,
) -> Result<String, EngineError> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted {
                run_id,
                workflow_type,
                ..
            } if run_id == continued_run_id => Some(workflow_type.clone()),
            _ => None,
        })
        .ok_or_else(|| EngineError::Load {
            reason: format!(
                "workflow `{workflow_id}` continued from run `{continued_run_id}` \
                 but that run has no WorkflowStarted event in durable history"
            ),
        })
}

#[cfg(test)]
#[path = "startup_sweeps_tests.rs"]
mod tests;
