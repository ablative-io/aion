//! Start path: spawn, `WorkflowStarted`, and register.

use std::sync::Arc;

use aion_core::{Payload, RunId, WorkflowId, WorkflowStatus};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;

use super::visibility::upsert_workflow_visibility;
use crate::EngineError;
use crate::durability::Recorder;
use crate::loader::LoadedWorkflows;
use crate::registry::{
    CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
};
use crate::runtime::{RuntimeHandle, RuntimeInput};
use crate::supervision::{SupervisionTree, spawn_workflow_with_policy};

/// Dependencies required to start one workflow execution.
pub struct StartWorkflowContext<'a> {
    /// Durable event store used by the workflow's single recorder.
    pub store: Arc<dyn EventStore>,
    /// Visibility index updated after state-changing workflow events.
    pub visibility_store: Arc<dyn VisibilityStore>,
    /// Loader-owned workflow records keyed by logical workflow type and version.
    pub loaded_workflows: &'a LoadedWorkflows,
    /// Runtime boundary used to spawn the workflow process.
    pub runtime: &'a RuntimeHandle,
    /// Structural supervision tree recording the per-type supervisor placement.
    pub supervision: &'a SupervisionTree,
    /// Active execution registry keyed by workflow/run identifiers.
    pub registry: &'a Registry,
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
    let loaded = context
        .loaded_workflows
        .latest(workflow_type)
        .ok_or_else(|| EngineError::WorkflowNotFound {
            workflow_type: workflow_type.to_owned(),
        })?;

    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&context.store))
        .with_visibility(run_id.clone(), Arc::clone(&context.visibility_store));
    recorder
        .record_workflow_started(Utc::now(), workflow_type.to_owned(), input.clone())
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
        context.runtime,
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

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload};
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::{StartWorkflowContext, start_workflow};
    use crate::EngineError;
    use crate::loader::LoadedWorkflows;
    use crate::registry::{HandleResidency, Registry};
    use crate::runtime::{RuntimeConfig, RuntimeHandle};
    use crate::supervision::SupervisionTree;

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn load_without_runtime_registration(workflow_type: &str) -> LoadedWorkflows {
        let mut loaded = LoadedWorkflows::new();
        loaded.note_loaded_workflow_for_test(
            workflow_type,
            format!("{workflow_type}__deployed"),
            "run",
            ContentHash::from_bytes([3; 32]),
        );
        loaded
    }

    fn context<'a>(
        store: Arc<dyn EventStore>,
        visibility_store: Arc<dyn VisibilityStore>,
        loaded_workflows: &'a LoadedWorkflows,
        runtime: &'a RuntimeHandle,
        supervision: &'a SupervisionTree,
        registry: &'a Registry,
    ) -> StartWorkflowContext<'a> {
        StartWorkflowContext {
            store,
            visibility_store,
            loaded_workflows,
            runtime,
            supervision,
            registry,
        }
    }

    #[tokio::test]
    async fn unknown_workflow_type_returns_not_found_and_appends_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(InMemoryStore::default());
        let loaded = LoadedWorkflows::new();
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        let supervision = SupervisionTree::new();
        let registry = Registry::default();
        let input = payload("input")?;

        let result = start_workflow(
            context(
                store.clone(),
                store.clone(),
                &loaded,
                &runtime,
                &supervision,
                &registry,
            ),
            "checkout",
            input,
        )
        .await;

        assert!(matches!(
            result,
            Err(EngineError::WorkflowNotFound { workflow_type }) if workflow_type == "checkout"
        ));
        assert_eq!(store.list_active().await?, Vec::new());
        assert_eq!(registry.list()?.len(), 0);
        runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn recorder_append_happens_before_spawn_failure() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = Arc::new(InMemoryStore::default());
        let loaded = load_without_runtime_registration("checkout");
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        let supervision = SupervisionTree::new();
        let registry = Registry::default();
        let input = payload("input")?;

        let result = start_workflow(
            context(
                store.clone(),
                store.clone(),
                &loaded,
                &runtime,
                &supervision,
                &registry,
            ),
            "checkout",
            input.clone(),
        )
        .await;

        assert!(matches!(result, Err(EngineError::Runtime { .. })));
        let active = store.list_active().await?;
        assert_eq!(active.len(), 1);
        let history = store.read_history(&active[0]).await?;
        assert_eq!(history.len(), 1);
        match &history[0] {
            Event::WorkflowStarted {
                envelope,
                workflow_type,
                input: recorded_input,
            } => {
                assert_eq!(envelope.seq, 1);
                assert_eq!(&envelope.workflow_id, &active[0]);
                assert_eq!(workflow_type, "checkout");
                assert_eq!(recorded_input, &input);
            }
            other => return Err(format!("expected WorkflowStarted, found {other:?}").into()),
        }
        assert_eq!(registry.list()?.len(), 0);
        runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn successful_start_appends_spawns_places_registers_and_returns_handle()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(InMemoryStore::default());
        let deployed_module = "checkout__deployed";
        let loaded = load_without_runtime_registration("checkout");
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        runtime.register_waiting_test_module(deployed_module, "run");
        let supervision = SupervisionTree::new();
        let registry = Registry::default();
        let input = payload("input")?;

        let handle = start_workflow(
            context(
                store.clone(),
                store.clone(),
                &loaded,
                &runtime,
                &supervision,
                &registry,
            ),
            "checkout",
            input.clone(),
        )
        .await?;

        assert_eq!(handle.workflow_type(), "checkout");
        assert_eq!(handle.loaded_version(), &ContentHash::from_bytes([3; 32]));
        assert_eq!(handle.cached_status(), aion_core::WorkflowStatus::Running);
        assert_eq!(handle.residency(), HandleResidency::Resident);
        assert!(!handle.completion().is_completed());
        runtime.wait_for_process_ready(handle.pid())?;
        assert!(runtime.trap_exit(handle.pid())?);
        assert_eq!(
            supervision
                .workflow(handle.pid())?
                .map(|node| node.workflow_pid()),
            Some(handle.pid())
        );

        let registered = registry.get(handle.workflow_id(), handle.run_id())?;
        assert_eq!(registered, Some(handle.clone()));
        let active = store.list_active().await?;
        assert_eq!(active, vec![handle.workflow_id().clone()]);
        let history = store.read_history(handle.workflow_id()).await?;
        assert_eq!(history.len(), 1);
        match &history[0] {
            Event::WorkflowStarted {
                envelope,
                workflow_type,
                input: recorded_input,
            } => {
                assert_eq!(envelope.seq, 1);
                assert_eq!(&envelope.workflow_id, handle.workflow_id());
                assert_eq!(workflow_type, "checkout");
                assert_eq!(recorded_input, &input);
            }
            other => return Err(format!("expected WorkflowStarted, found {other:?}").into()),
        }
        runtime.shutdown()?;
        Ok(())
    }
}
