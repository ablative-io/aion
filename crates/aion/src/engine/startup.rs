//! Startup recovery wiring used by `EngineBuilder::build()`: schedule
//! coordinator bootstrap, active-workflow repopulation, and timer recovery.

use std::{sync::Arc, time::Duration};

use chrono::Utc;

use aion_core::{
    Event, Payload, RunId, SearchAttributeSchema, WorkflowFilter, WorkflowStatus,
    status_from_events,
};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;

use crate::{
    CompletionNotifier, EngineError, HandleResidency, LoadedWorkflows, Registry, RuntimeHandle,
    SupervisionTree, WorkflowHandle, WorkflowHandleParts,
    durability::{
        ActiveWorkflowRecovery, ActiveWorkflowRecoverySeam, ActiveWorkflowRecoverySeamImpl,
        Recorder, current_run_segment,
    },
    lifecycle::completion::{ProcessExitContext, handle_process_exit},
    lifecycle::start::{StartWorkflowContext, StartWorkflowOptions, start_workflow_with_options},
    time::TimerRecovery,
};

use super::api_schedule::{
    schedule_coordinator_run_id, schedule_coordinator_workflow_id,
    schedule_coordinator_workflow_type,
};

pub(super) async fn recover_timers_on_startup(
    nif_state: &crate::runtime::EngineNifState,
    store: Arc<dyn EventStore>,
) -> Result<(), EngineError> {
    let readable_store: Arc<dyn aion_store::ReadableEventStore> = store;
    let timer_service = crate::runtime::nif_timer_bridge::installed_timer_service(nif_state)
        .map_err(|error| EngineError::Runtime {
            reason: format!("timer recovery service unavailable: {error}"),
        })?;
    TimerRecovery::new(readable_store, timer_service, Duration::ZERO)
        .recover_on_startup(Utc::now())
        .await
        .map(|_| ())
        .map_err(|error| EngineError::Runtime {
            reason: format!("timer recovery failed: {error}"),
        })
}

pub(super) struct StartupRecoveryContext<'a> {
    pub(super) store: Arc<dyn EventStore>,
    pub(super) visibility_store: Arc<dyn VisibilityStore>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) loaded_workflows: &'a LoadedWorkflows,
    pub(super) registry: Arc<Registry>,
    pub(super) supervision: Arc<SupervisionTree>,
    pub(super) recovery: Option<Arc<dyn ActiveWorkflowRecoverySeam>>,
    pub(super) search_attribute_schema: Arc<SearchAttributeSchema>,
}

pub(super) async fn recover_active_workflows_on_startup(
    context: StartupRecoveryContext<'_>,
) -> Result<(), EngineError> {
    bootstrap_schedule_coordinator(Arc::clone(&context.store)).await?;
    crate::lifecycle::visibility::reconcile_visibility(
        Arc::clone(&context.store),
        Arc::clone(&context.visibility_store),
    )
    .await?;
    let recovery = context.recovery.clone().unwrap_or_else(|| {
        Arc::new(ActiveWorkflowRecoverySeamImpl::new(Arc::clone(
            &context.runtime,
        ))) as Arc<dyn ActiveWorkflowRecoverySeam>
    });
    repopulate_active_workflows(&context, recovery.as_ref()).await?;
    sweep_continued_as_new_replacements(&context).await
}

async fn bootstrap_schedule_coordinator(store: Arc<dyn EventStore>) -> Result<(), EngineError> {
    let workflow_id = schedule_coordinator_workflow_id();
    let history = store.as_ref().read_history(&workflow_id).await?;
    if !history.is_empty() {
        return Ok(());
    }

    let input = Payload::from_json(&serde_json::json!({})).map_err(|error| EngineError::Load {
        reason: format!("failed to build schedule coordinator input payload: {error}"),
    })?;
    let run_id = schedule_coordinator_run_id();
    let mut recorder = Recorder::new(workflow_id, store);
    recorder
        .record_workflow_started(
            Utc::now(),
            schedule_coordinator_workflow_type().to_owned(),
            input,
            run_id,
        )
        .await?;
    Ok(())
}

async fn repopulate_active_workflows(
    context: &StartupRecoveryContext<'_>,
    recovery: &dyn ActiveWorkflowRecoverySeam,
) -> Result<(), EngineError> {
    let store = &context.store;
    let visibility_store = &context.visibility_store;
    let runtime = &context.runtime;
    let loaded_workflows = context.loaded_workflows;
    let registry = &context.registry;
    let supervision = &context.supervision;
    for workflow_id in store.as_ref().list_active().await? {
        let history = store.as_ref().read_history(&workflow_id).await?;
        let workflow_type = started_workflow_type(&workflow_id, &history)?;
        let projected_status = status_from_events(&history);
        if projected_status.is_terminal() {
            tracing::warn!(
                workflow_id = %workflow_id,
                status = ?projected_status,
                "store listed terminal workflow as active during startup; skipping resident recovery"
            );
            continue;
        }
        supervision.ensure_type_supervisor(workflow_type.clone())?;

        let recovered = recover_active_workflow(
            recovery,
            &workflow_id,
            &workflow_type,
            &history,
            loaded_workflows,
        )?;
        let history_head = history.last().map(Event::seq).unwrap_or_default();
        match recovered {
            ActiveWorkflowRecovery::Resident {
                run_id,
                loaded_version,
                pid,
            } => {
                let recorder =
                    Recorder::resume_at(workflow_id.clone(), Arc::clone(store), history_head)
                        .with_visibility(run_id.clone(), Arc::clone(visibility_store));
                let completion = CompletionNotifier::new();
                let handle = WorkflowHandle::new(WorkflowHandleParts {
                    workflow_id: workflow_id.clone(),
                    run_id: run_id.clone(),
                    pid,
                    workflow_type: workflow_type.clone(),
                    loaded_version,
                    cached_status: projected_status,
                    residency: HandleResidency::Resident,
                    recorder,
                    completion,
                });
                if let Err(error) = registry
                    .insert((workflow_id.clone(), run_id.clone()), handle.clone())
                    .and_then(|_| registry.reconcile(&workflow_id, &run_id, &history))
                    .and_then(|_| {
                        supervision
                            .place_workflow(workflow_type.clone(), pid)
                            .map(|_| ())
                    })
                    .and_then(|()| {
                        install_recovered_completion_monitor(
                            RecoveredMonitorParts {
                                store: Arc::clone(store),
                                visibility_store: Arc::clone(visibility_store),
                                runtime: Arc::clone(runtime),
                                registry: Arc::clone(registry),
                                loaded_workflows: Arc::new(loaded_workflows.clone()),
                                supervision: Arc::clone(supervision),
                                search_attribute_schema: Arc::clone(
                                    &context.search_attribute_schema,
                                ),
                            },
                            &handle,
                        )
                    })
                {
                    rollback_recovered_resident(
                        runtime,
                        registry,
                        &workflow_id,
                        &run_id,
                        pid,
                        &error,
                    );
                    return Err(error);
                }
                sweep_recorded_children(context, &workflow_id, &run_id, &history).await?;
            }
            ActiveWorkflowRecovery::ScheduleCoordinator { run_id } => {
                registry.reconcile(&workflow_id, &run_id, &history)?;
            }
        }
    }

    Ok(())
}

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
async fn sweep_recorded_children(
    context: &StartupRecoveryContext<'_>,
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
                loaded_workflows: context.loaded_workflows,
                runtime: Arc::clone(&context.runtime),
                supervision: Arc::clone(&context.supervision),
                registry: Arc::clone(&context.registry),
                signal_handoff: None,
                search_attribute_schema: Arc::clone(&context.search_attribute_schema),
            },
            workflow_type,
            input.clone(),
            StartWorkflowOptions {
                workflow_id: Some(child_workflow_id.clone()),
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
async fn sweep_continued_as_new_replacements(
    context: &StartupRecoveryContext<'_>,
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
        start_workflow_with_options(
            StartWorkflowContext {
                store: Arc::clone(&context.store),
                visibility_store: Arc::clone(&context.visibility_store),
                loaded_workflows: context.loaded_workflows,
                runtime: Arc::clone(&context.runtime),
                supervision: Arc::clone(&context.supervision),
                registry: Arc::clone(&context.registry),
                signal_handoff: None,
                search_attribute_schema: Arc::clone(&context.search_attribute_schema),
            },
            &replacement_type,
            input,
            StartWorkflowOptions {
                workflow_id: Some(workflow_id.clone()),
                parent_run_id: Some(continued_run_id),
                // Recorded attributes carry into the replacement run's
                // projection, exactly as in the monitor's replacement start.
                ..StartWorkflowOptions::default()
            },
        )
        .await?;
    }
    Ok(())
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

struct RecoveredMonitorParts {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    runtime: Arc<RuntimeHandle>,
    registry: Arc<Registry>,
    loaded_workflows: Arc<LoadedWorkflows>,
    supervision: Arc<SupervisionTree>,
    search_attribute_schema: Arc<SearchAttributeSchema>,
}

fn install_recovered_completion_monitor(
    parts: RecoveredMonitorParts,
    handle: &WorkflowHandle,
) -> Result<(), EngineError> {
    let pid = handle.pid();
    let runtime = Arc::clone(&parts.runtime);
    let completion_context = ProcessExitContext {
        store: parts.store,
        visibility_store: parts.visibility_store,
        registry: parts.registry,
        loaded_workflows: parts.loaded_workflows,
        runtime: parts.runtime,
        supervision: parts.supervision,
        tokio_handle: tokio::runtime::Handle::current(),
        search_attribute_schema: parts.search_attribute_schema,
    };
    let completion_handle = handle.clone();
    runtime.monitor_process(pid, move |outcome| {
        if let Err(error) = handle_process_exit(completion_context, completion_handle, outcome) {
            tracing::error!(workflow_pid = pid, error = %error, "recovered workflow process monitor completion failed");
        }
    })?;
    Ok(())
}

fn rollback_recovered_resident(
    runtime: &RuntimeHandle,
    registry: &Registry,
    workflow_id: &aion_core::WorkflowId,
    run_id: &RunId,
    pid: crate::Pid,
    cause: &EngineError,
) {
    if let Err(error) = registry.remove(workflow_id, run_id) {
        tracing::warn!(workflow_id = %workflow_id, error = %error, "failed to roll back recovered workflow registry entry");
    }
    if let Err(error) = runtime.cancel_pid(pid) {
        tracing::warn!(workflow_id = %workflow_id, pid, error = %error, cause = %cause, "failed to cancel recovered workflow process after startup registration failed");
    }
}

fn recover_active_workflow(
    recovery: &dyn ActiveWorkflowRecoverySeam,
    workflow_id: &aion_core::WorkflowId,
    workflow_type: &str,
    history: &[Event],
    loaded_workflows: &LoadedWorkflows,
) -> Result<ActiveWorkflowRecovery, EngineError> {
    if workflow_id == &schedule_coordinator_workflow_id()
        && workflow_type == schedule_coordinator_workflow_type()
    {
        let run_id = started_run_id(workflow_id, history)?;
        return Ok(ActiveWorkflowRecovery::ScheduleCoordinator { run_id });
    }

    recovery.recover_active_workflow(workflow_id, workflow_type, history, loaded_workflows)
}

fn started_workflow_type(
    workflow_id: &aion_core::WorkflowId,
    history: &[Event],
) -> Result<String, EngineError> {
    if let Some(workflow_type) = history.iter().find_map(|event| match event {
        Event::WorkflowStarted { workflow_type, .. } => Some(workflow_type.clone()),
        _ => None,
    }) {
        return Ok(workflow_type);
    }

    if workflow_id == &schedule_coordinator_workflow_id() {
        return Ok(schedule_coordinator_workflow_type().to_owned());
    }

    Err(EngineError::Load {
        reason: format!(
            "active workflow `{workflow_id}` has no WorkflowStarted event in durable history"
        ),
    })
}

fn started_run_id(
    workflow_id: &aion_core::WorkflowId,
    history: &[Event],
) -> Result<RunId, EngineError> {
    if let Some(run_id) = history.iter().find_map(|event| match event {
        Event::WorkflowStarted { run_id, .. } => Some(run_id.clone()),
        _ => None,
    }) {
        return Ok(run_id);
    }

    if workflow_id == &schedule_coordinator_workflow_id() {
        return Ok(schedule_coordinator_run_id());
    }

    Err(EngineError::Load {
        reason: format!(
            "active workflow `{workflow_id}` has no WorkflowStarted run id in durable history"
        ),
    })
}
