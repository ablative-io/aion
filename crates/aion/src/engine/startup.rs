//! Startup recovery wiring used by `EngineBuilder::build()`: schedule
//! coordinator bootstrap, active-workflow repopulation, and timer recovery.

use std::{sync::Arc, time::Duration};

use chrono::Utc;

use aion_core::{Event, Payload, RunId, SearchAttributeSchema, WorkflowStatus, status_from_events};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;

use crate::{
    CompletionNotifier, EngineError, HandleResidency, Registry, RuntimeHandle, SupervisionTree,
    WorkflowCatalog, WorkflowHandle, WorkflowHandleParts,
    durability::{
        ActiveWorkflowRecovery, ActiveWorkflowRecoverySeam, ActiveWorkflowRecoverySeamImpl,
        Recorder,
    },
    lifecycle::completion::{ProcessExitContext, handle_process_exit},
    time::TimerRecovery,
};

use super::api_schedule::{
    schedule_coordinator_package_version, schedule_coordinator_run_id,
    schedule_coordinator_workflow_id, schedule_coordinator_workflow_type,
};
use super::startup_sweeps::{sweep_continued_as_new_replacements, sweep_recorded_children};

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

pub(super) struct StartupRecoveryContext {
    pub(super) store: Arc<dyn EventStore>,
    pub(super) visibility_store: Arc<dyn VisibilityStore>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) catalog: Arc<WorkflowCatalog>,
    pub(super) registry: Arc<Registry>,
    pub(super) supervision: Arc<SupervisionTree>,
    pub(super) recovery: Option<Arc<dyn ActiveWorkflowRecoverySeam>>,
    pub(super) search_attribute_schema: Arc<SearchAttributeSchema>,
}

pub(super) async fn recover_active_workflows_on_startup(
    context: StartupRecoveryContext,
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
            crate::durability::WorkflowStartRecord {
                workflow_type: schedule_coordinator_workflow_type().to_owned(),
                input,
                run_id,
                parent_run_id: None,
                package_version: schedule_coordinator_package_version(),
            },
        )
        .await?;
    Ok(())
}

async fn repopulate_active_workflows(
    context: &StartupRecoveryContext,
    recovery: &dyn ActiveWorkflowRecoverySeam,
) -> Result<(), EngineError> {
    let store = &context.store;
    let catalog = &context.catalog;
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

        // Per-workflow isolation (#62): a run whose pinned package version
        // (or replay metadata) cannot be resolved fails its own recovery with
        // a typed error, logged here; it must not abort the engine build or
        // other workflows' recovery.
        let recovered = match recover_active_workflow(
            recovery,
            &workflow_id,
            &workflow_type,
            &history,
            catalog,
        ) {
            Ok(recovered) => recovered,
            Err(error) => {
                tracing::error!(
                    workflow_id = %workflow_id,
                    workflow_type = %workflow_type,
                    error = %error,
                    "active workflow failed startup recovery; skipping it and continuing"
                );
                continue;
            }
        };
        let history_head = history.last().map(Event::seq).unwrap_or_default();
        match recovered {
            ActiveWorkflowRecovery::Resident {
                run_id,
                loaded_version,
                pid,
            } => {
                register_recovered_resident(
                    context,
                    RecoveredResident {
                        workflow_id: &workflow_id,
                        workflow_type: &workflow_type,
                        history: &history,
                        history_head,
                        projected_status,
                        run_id,
                        loaded_version,
                        pid,
                    },
                )
                .await?;
            }
            ActiveWorkflowRecovery::ScheduleCoordinator { run_id } => {
                registry.reconcile(&workflow_id, &run_id, &history)?;
            }
        }
    }

    Ok(())
}

/// One resident workflow recovered by the AD seam, ready for registration.
struct RecoveredResident<'a> {
    workflow_id: &'a aion_core::WorkflowId,
    workflow_type: &'a str,
    history: &'a [Event],
    history_head: u64,
    projected_status: WorkflowStatus,
    run_id: RunId,
    loaded_version: aion_package::ContentHash,
    pid: crate::Pid,
}

/// Register one recovered resident process: recorder, registry, supervision,
/// completion monitor, and the recorded-children crash-window sweep.
async fn register_recovered_resident(
    context: &StartupRecoveryContext,
    resident: RecoveredResident<'_>,
) -> Result<(), EngineError> {
    let RecoveredResident {
        workflow_id,
        workflow_type,
        history,
        history_head,
        projected_status,
        run_id,
        loaded_version,
        pid,
    } = resident;
    let recorder = Recorder::resume_at(
        workflow_id.clone(),
        Arc::clone(&context.store),
        history_head,
    )
    .with_visibility(run_id.clone(), Arc::clone(&context.visibility_store));
    let completion = CompletionNotifier::new();
    let handle = WorkflowHandle::new(WorkflowHandleParts {
        workflow_id: workflow_id.clone(),
        run_id: run_id.clone(),
        pid,
        workflow_type: workflow_type.to_owned(),
        namespace: String::from("default"),
        loaded_version,
        cached_status: projected_status,
        residency: HandleResidency::Resident,
        recorder,
        completion,
    });
    if let Err(error) = context
        .registry
        .insert((workflow_id.clone(), run_id.clone()), handle.clone())
        .and_then(|_| context.registry.reconcile(workflow_id, &run_id, history))
        .and_then(|_| {
            context
                .supervision
                .place_workflow(workflow_type.to_owned(), pid)
                .map(|_| ())
        })
        .and_then(|()| {
            install_recovered_completion_monitor(
                RecoveredMonitorParts {
                    store: Arc::clone(&context.store),
                    visibility_store: Arc::clone(&context.visibility_store),
                    runtime: Arc::clone(&context.runtime),
                    registry: Arc::clone(&context.registry),
                    catalog: Arc::clone(&context.catalog),
                    supervision: Arc::clone(&context.supervision),
                    search_attribute_schema: Arc::clone(&context.search_attribute_schema),
                },
                &handle,
            )
        })
    {
        rollback_recovered_resident(
            &context.runtime,
            &context.registry,
            workflow_id,
            &run_id,
            pid,
            &error,
        );
        return Err(error);
    }
    sweep_recorded_children(context, workflow_id, &run_id, history).await
}

struct RecoveredMonitorParts {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    runtime: Arc<RuntimeHandle>,
    registry: Arc<Registry>,
    catalog: Arc<WorkflowCatalog>,
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
        catalog: parts.catalog,
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
    catalog: &WorkflowCatalog,
) -> Result<ActiveWorkflowRecovery, EngineError> {
    if workflow_id == &schedule_coordinator_workflow_id()
        && workflow_type == schedule_coordinator_workflow_type()
    {
        let run_id = started_run_id(workflow_id, history)?;
        return Ok(ActiveWorkflowRecovery::ScheduleCoordinator { run_id });
    }

    recovery.recover_active_workflow(workflow_id, workflow_type, history, catalog)
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

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
