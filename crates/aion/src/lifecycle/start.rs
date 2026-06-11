//! Start path: spawn, `WorkflowStarted`, and register.

use std::collections::HashMap;
use std::sync::Arc;

use aion_core::{
    Event, Payload, RunId, SearchAttributeSchema, SearchAttributeValue, WorkflowId, WorkflowStatus,
};
use aion_package::ContentHash;
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;

use super::completion::{ProcessExitContext, handle_process_exit};
use super::visibility::upsert_workflow_visibility;
use crate::durability::Recorder;
use crate::loader::LoadedWorkflows;
use crate::registry::{
    CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
};
use crate::runtime::{RuntimeHandle, RuntimeInput};
use crate::supervision::{SupervisionTree, spawn_workflow_with_policy};
use crate::{
    EngineError,
    engine_seam::{
        ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
        TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
    },
    signal::SignalResumeHandoff,
};

/// Dependencies required to start one workflow execution.
pub struct StartWorkflowContext<'a> {
    /// Durable event store used by the workflow's single recorder.
    pub store: Arc<dyn EventStore>,
    /// Visibility index updated after state-changing workflow events.
    pub visibility_store: Arc<dyn VisibilityStore>,
    /// Loader-owned workflow records keyed by logical workflow type and version.
    pub loaded_workflows: &'a LoadedWorkflows,
    /// Runtime boundary used to spawn the workflow process.
    pub runtime: Arc<RuntimeHandle>,
    /// Structural supervision tree recording the per-type supervisor placement.
    pub supervision: Arc<SupervisionTree>,
    /// Active execution registry keyed by workflow/run identifiers.
    pub registry: Arc<Registry>,
    /// Shared non-resident signal handoff to flush after resident registration.
    pub signal_handoff: Option<Arc<SignalResumeHandoff>>,
    /// Schema validating any initial search attributes before they are recorded.
    pub search_attribute_schema: Arc<SearchAttributeSchema>,
    /// Tokio handle the spawned workflow's exit monitor captures for its
    /// completion work. Must be epoch-stable — the host runtime's handle,
    /// never an engine-owned task runtime's — because the monitor outlives
    /// the start call and blocks on this handle when the process exits.
    pub monitor_tokio_handle: tokio::runtime::Handle,
}

/// Optional identifiers used by internal start callers such as continue-as-new.
#[derive(Clone, Debug, Default)]
pub struct StartWorkflowOptions {
    /// Existing workflow identifier to reuse; omitted for a fresh workflow.
    pub workflow_id: Option<WorkflowId>,
    /// Parent run that continued into this run, when applicable.
    pub parent_run_id: Option<RunId>,
    /// Exact loaded package version to spawn; omitted to use the latest version.
    pub loaded_version: Option<ContentHash>,
    /// Initial search attributes recorded atomically with `WorkflowStarted`.
    pub search_attributes: HashMap<String, SearchAttributeValue>,
}

/// Starts a loaded workflow execution and returns its active handle.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] before appending anything when
/// `workflow_type` is not loaded. Recorder failures surface as
/// [`EngineError::Durability`] and stop before any process is spawned. Runtime,
/// supervision, and registry failures surface as their typed [`EngineError`]
/// variants.
pub async fn start_workflow(
    context: StartWorkflowContext<'_>,
    workflow_type: &str,
    input: Payload,
) -> Result<WorkflowHandle, EngineError> {
    start_workflow_with_options(
        context,
        workflow_type,
        input,
        StartWorkflowOptions::default(),
    )
    .await
}

/// Starts a loaded workflow execution with caller-supplied lifecycle options.
///
/// # Errors
///
/// Returns the same typed errors as [`start_workflow`].
pub async fn start_workflow_with_options(
    context: StartWorkflowContext<'_>,
    workflow_type: &str,
    input: Payload,
    options: StartWorkflowOptions,
) -> Result<WorkflowHandle, EngineError> {
    let loaded = match &options.loaded_version {
        Some(version) => context.loaded_workflows.get(workflow_type, version),
        None => context.loaded_workflows.latest(workflow_type),
    }
    .ok_or_else(|| EngineError::WorkflowNotFound {
        workflow_type: workflow_type.to_owned(),
    })?;

    let supplied_workflow_id = options.workflow_id.is_some();
    let workflow_id = options.workflow_id.unwrap_or_else(WorkflowId::new_v4);
    let run_id = RunId::new_v4();
    let initial_head = if supplied_workflow_id {
        context
            .store
            .read_history(&workflow_id)
            .await?
            .iter()
            .map(Event::seq)
            .max()
            .unwrap_or_default()
    } else {
        0
    };
    let mut recorder = Recorder::resume_at(
        workflow_id.clone(),
        Arc::clone(&context.store),
        initial_head,
    )
    .with_visibility(run_id.clone(), Arc::clone(&context.visibility_store));
    recorder
        .record_workflow_started_with_attributes(
            Utc::now(),
            crate::durability::WorkflowStartRecord {
                workflow_type: workflow_type.to_owned(),
                input: input.clone(),
                run_id: run_id.clone(),
                parent_run_id: options.parent_run_id,
                package_version: crate::loader::package_version_of(loaded.version()),
            },
            options.search_attributes,
            &context.search_attribute_schema,
        )
        .await?;
    upsert_workflow_visibility(
        Arc::clone(&context.store),
        Arc::clone(&context.visibility_store),
        &workflow_id,
        &run_id,
    )
    .await?;

    context
        .supervision
        .ensure_type_supervisor(loaded.workflow_type())?;
    let runtime_input = RuntimeInput::from_payload(&input)?;
    let pid = spawn_workflow_with_policy(
        &context.runtime,
        loaded.deployed_entry_module(),
        loaded.entry_function(),
        runtime_input,
    )?;
    if let Err(error) = context
        .supervision
        .place_workflow(loaded.workflow_type(), pid)
    {
        let _ = context.runtime.cancel_pid(pid);
        return Err(error);
    }

    let completion = CompletionNotifier::new();
    let handle = WorkflowHandle::new(WorkflowHandleParts {
        workflow_id: workflow_id.clone(),
        run_id: run_id.clone(),
        pid,
        workflow_type: loaded.workflow_type().to_owned(),
        loaded_version: loaded.version().clone(),
        cached_status: WorkflowStatus::Running,
        residency: HandleResidency::Resident,
        recorder,
        completion,
    });

    if let Err(error) = context
        .registry
        .insert((workflow_id, run_id), handle.clone())
        .map(|_| ())
    {
        let _ = context.runtime.cancel_pid(pid);
        return Err(error);
    }

    if let Err(error) = install_completion_monitor(&context, pid, &handle) {
        let _ = context
            .registry
            .remove(handle.workflow_id(), handle.run_id());
        let _ = context.runtime.cancel_pid(pid);
        return Err(error);
    }
    deliver_deferred_signals(&context, &handle);

    Ok(handle)
}

fn install_completion_monitor(
    context: &StartWorkflowContext<'_>,
    pid: crate::Pid,
    handle: &WorkflowHandle,
) -> Result<(), EngineError> {
    let completion_context = ProcessExitContext {
        store: Arc::clone(&context.store),
        visibility_store: Arc::clone(&context.visibility_store),
        registry: Arc::clone(&context.registry),
        loaded_workflows: Arc::new(context.loaded_workflows.clone()),
        runtime: Arc::clone(&context.runtime),
        supervision: Arc::clone(&context.supervision),
        // Epoch-stable by the StartWorkflowContext contract: the monitor
        // fires whenever the process exits — potentially long after the
        // start call's own executor is gone — so it must never capture
        // `Handle::current()` of an engine-owned task runtime.
        tokio_handle: context.monitor_tokio_handle.clone(),
        search_attribute_schema: Arc::clone(&context.search_attribute_schema),
    };
    let completion_handle = handle.clone();
    context.runtime.monitor_process(pid, move |outcome| {
        if let Err(error) = handle_process_exit(completion_context, completion_handle, outcome) {
            tracing::error!(workflow_pid = pid, error = %error, "workflow process monitor completion failed");
        }
    })?;
    Ok(())
}

fn deliver_deferred_signals(context: &StartWorkflowContext<'_>, handle: &WorkflowHandle) {
    let Some(handoff) = &context.signal_handoff else {
        return;
    };
    let adapter = StartResumeEngineHandle {
        runtime: &context.runtime,
        registry: &context.registry,
    };
    if let Err(error) = handoff.deliver_deferred(&adapter, handle.workflow_id()) {
        tracing::warn!(
            workflow_id = %handle.workflow_id(),
            error = %error,
            "failed to flush deferred signals after workflow became resident"
        );
    }
}

struct StartResumeEngineHandle<'a> {
    runtime: &'a RuntimeHandle,
    registry: &'a Registry,
}

impl EngineHandle for StartResumeEngineHandle<'_> {
    fn resolve_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowResidency, EngineSeamError> {
        let handle = self
            .registry
            .list()
            .map_err(|error| EngineSeamError::Delivery {
                reason: error.to_string(),
            })?
            .into_iter()
            .find(|handle| handle.workflow_id() == workflow_id);
        match handle {
            Some(handle) if handle.residency() == HandleResidency::Resident => Ok(
                WorkflowResidency::Resident(WorkflowProcessHandle::new(handle.pid())),
            ),
            Some(_) => Ok(WorkflowResidency::NonResident),
            None => Ok(WorkflowResidency::Unknown),
        }
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        match message {
            WorkflowMailboxMessage::SignalReceived { .. } => self
                .runtime
                .deliver_signal_received(process.pid())
                .map_err(|error| EngineSeamError::Delivery {
                    reason: error.to_string(),
                }),
            other => Err(EngineSeamError::Delivery {
                reason: format!("unsupported resume handoff message: {other:?}"),
            }),
        }
    }

    fn spawn_child_workflow(
        &self,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
        let _ = request;
        Err(EngineSeamError::ChildSpawn {
            reason: "start resume handoff cannot spawn child workflows".to_owned(),
        })
    }

    fn terminate_linked_child_workflow(
        &self,
        parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        let _ = (parent_workflow_id, child_process, correlation);
        Err(EngineSeamError::ChildTermination {
            reason: "start resume handoff cannot terminate child workflows".to_owned(),
        })
    }

    fn terminate_linked_activity(
        &self,
        parent_workflow_id: &WorkflowId,
        activity_process: crate::Pid,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        let _ = (parent_workflow_id, activity_process, correlation);
        Err(EngineSeamError::ChildTermination {
            reason: "start resume handoff cannot terminate activities".to_owned(),
        })
    }

    fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
        let _ = entry;
        Err(EngineSeamError::TimerWheel {
            reason: "start resume handoff cannot arm timers".to_owned(),
        })
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &aion_core::TimerId,
    ) -> Result<(), EngineSeamError> {
        let _ = (process, timer_id);
        Err(EngineSeamError::TimerWheel {
            reason: "start resume handoff cannot disarm timers".to_owned(),
        })
    }

    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: Event,
    ) -> Result<(), EngineSeamError> {
        let _ = (workflow_id, event);
        Err(EngineSeamError::Recorder {
            reason: "start resume handoff cannot record workflow events".to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod start_tests;
