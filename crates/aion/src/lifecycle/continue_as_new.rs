//! Continue-as-new lifecycle transition.

use std::collections::HashSet;
use std::sync::Arc;

use aion_core::{ActivityId, Event, Payload, RunId, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;

use crate::EngineError;
use crate::lifecycle::start::{self, StartWorkflowContext, StartWorkflowOptions};
use crate::loader::LoadedWorkflows;
use crate::registry::{Registry, TerminalOutcome, WorkflowHandle};
use crate::runtime::RuntimeHandle;
use crate::supervision::SupervisionTree;

/// Dependencies required to continue a workflow as a new run.
pub struct ContinueAsNewContext<'a> {
    /// Durable event store used to scan history and start the replacement run.
    pub store: Arc<dyn EventStore>,
    /// Visibility store for workflow visibility projections.
    pub visibility_store: Arc<dyn VisibilityStore>,
    /// Loader-owned workflow records keyed by logical workflow type and version.
    pub loaded_workflows: &'a LoadedWorkflows,
    /// Runtime boundary used to spawn the replacement workflow process.
    pub runtime: &'a RuntimeHandle,
    /// Structural supervision tree recording the per-type supervisor placement.
    pub supervision: &'a SupervisionTree,
    /// Active execution registry keyed by workflow/run identifiers.
    pub registry: &'a Registry,
}

/// Request payload carried into the replacement run.
#[derive(Clone, Debug, PartialEq)]
pub struct ContinueAsNewRequest {
    /// Opaque workflow input payload for the replacement run.
    pub input: Payload,
    /// Optional workflow type override for the replacement run.
    pub workflow_type: Option<String>,
}

/// Continues a live workflow run as a new run under the same workflow id.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair is
/// not registered. Returns [`EngineError::Runtime`] when pending activities or
/// child workflows remain unresolved. Recorder, runtime, supervision, and
/// registry failures surface as their typed [`EngineError`] variants.
pub async fn continue_as_new(
    context: ContinueAsNewContext<'_>,
    id: &WorkflowId,
    run: &RunId,
    request: ContinueAsNewRequest,
) -> Result<WorkflowHandle, EngineError> {
    let handle = registered_handle(context.registry, id, run)?;
    let history = context.store.read_history(id).await?;
    guard_no_pending_work(&history)?;

    let workflow_type = request
        .workflow_type
        .as_deref()
        .unwrap_or(handle.workflow_type());
    validate_replacement_workflow_type(context.loaded_workflows, workflow_type)?;

    {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        recorder
            .record_workflow_continued_as_new(
                Utc::now(),
                request.input.clone(),
                request.workflow_type.clone(),
                run.clone(),
            )
            .await?;
    }

    let new_handle = start::start_workflow_with_options(
        StartWorkflowContext {
            store: context.store,
            visibility_store: context.visibility_store,
            loaded_workflows: context.loaded_workflows,
            runtime: context.runtime,
            supervision: context.supervision,
            registry: context.registry,
        },
        workflow_type,
        request.input.clone(),
        StartWorkflowOptions {
            workflow_id: Some(id.clone()),
            parent_run_id: Some(run.clone()),
        },
    )
    .await?;

    handle.completion().notify(TerminalOutcome::ContinuedAsNew {
        input: request.input.clone(),
        workflow_type: request.workflow_type.clone(),
        parent_run_id: run.clone(),
    });
    context.registry.remove(id, run)?;

    Ok(new_handle)
}

fn guard_no_pending_work(events: &[Event]) -> Result<(), EngineError> {
    let mut pending_activities = HashSet::<ActivityId>::new();
    let mut pending_children = HashSet::<WorkflowId>::new();

    for event in events {
        match event {
            Event::ActivityScheduled { activity_id, .. }
            | Event::ActivityStarted { activity_id, .. } => {
                pending_activities.insert(activity_id.clone());
            }
            Event::ActivityCompleted { activity_id, .. }
            | Event::ActivityFailed { activity_id, .. }
            | Event::ActivityCancelled { activity_id, .. } => {
                pending_activities.remove(activity_id);
            }
            Event::ChildWorkflowStarted {
                child_workflow_id, ..
            } => {
                pending_children.insert(child_workflow_id.clone());
            }
            Event::ChildWorkflowCompleted {
                child_workflow_id, ..
            }
            | Event::ChildWorkflowFailed {
                child_workflow_id, ..
            }
            | Event::ChildWorkflowCancelled {
                child_workflow_id, ..
            } => {
                pending_children.remove(child_workflow_id);
            }
            Event::WorkflowStarted { .. }
            | Event::WorkflowCompleted { .. }
            | Event::WorkflowFailed { .. }
            | Event::WorkflowCancelled { .. }
            | Event::WorkflowTimedOut { .. }
            | Event::WorkflowContinuedAsNew { .. }
            | Event::SearchAttributesUpdated { .. }
            | Event::TimerStarted { .. }
            | Event::TimerFired { .. }
            | Event::TimerCancelled { .. }
            | Event::SignalReceived { .. }
            | Event::ScheduleCreated { .. }
            | Event::ScheduleUpdated { .. }
            | Event::SchedulePaused { .. }
            | Event::ScheduleResumed { .. }
            | Event::ScheduleDeleted { .. }
            | Event::ScheduleTriggered { .. } => {}
        }
    }

    if pending_activities.is_empty() && pending_children.is_empty() {
        return Ok(());
    }

    Err(EngineError::Runtime {
        reason: format!(
            "cannot continue as new while pending work exists: {} activities ({:?}), {} child workflows ({:?})",
            pending_activities.len(),
            pending_activities,
            pending_children.len(),
            pending_children
        ),
    })
}

fn registered_handle(
    registry: &Registry,
    id: &WorkflowId,
    run: &RunId,
) -> Result<WorkflowHandle, EngineError> {
    registry
        .get(id, run)?
        .ok_or_else(|| EngineError::WorkflowNotFound {
            workflow_type: format!("{id}/{run}"),
        })
}

fn validate_replacement_workflow_type(
    loaded_workflows: &LoadedWorkflows,
    workflow_type: &str,
) -> Result<(), EngineError> {
    loaded_workflows
        .latest(workflow_type)
        .ok_or_else(|| EngineError::WorkflowNotFound {
            workflow_type: workflow_type.to_owned(),
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{ActivityId, Event, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::{ContinueAsNewContext, ContinueAsNewRequest, continue_as_new};
    use crate::EngineError;
    use crate::durability::Recorder;
    use crate::loader::LoadedWorkflows;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, TerminalOutcome, WorkflowHandle,
        WorkflowHandleParts,
    };
    use crate::runtime::{RuntimeConfig, RuntimeHandle};
    use crate::supervision::SupervisionTree;

    struct ActiveWorkflow {
        store: Arc<dyn EventStore>,
        visibility_store: Arc<dyn VisibilityStore>,
        loaded: LoadedWorkflows,
        runtime: RuntimeHandle,
        supervision: SupervisionTree,
        registry: Registry,
        handle: WorkflowHandle,
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn loaded_workflows() -> LoadedWorkflows {
        let mut loaded = LoadedWorkflows::new();
        loaded.note_loaded_workflow_for_test(
            "checkout",
            "checkout_deployed",
            "run",
            ContentHash::from_bytes([3; 32]),
        );
        loaded.note_loaded_workflow_for_test(
            "checkout-v2",
            "checkout_v2_deployed",
            "run",
            ContentHash::from_bytes([4; 32]),
        );
        loaded
    }

    async fn active_workflow() -> Result<ActiveWorkflow, Box<dyn std::error::Error>> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let loaded = loaded_workflows();
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        runtime.register_waiting_test_module("checkout_deployed", "run");
        runtime.register_waiting_test_module("checkout_v2_deployed", "run");
        let supervision = SupervisionTree::new();
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&store));
        recorder
            .record_workflow_started(chrono::Utc::now(), "checkout".to_owned(), payload("input")?)
            .await?;
        let pid = runtime.spawn_test_process_with_trap_exit(true)?;
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "checkout".to_owned(),
            loaded_version: ContentHash::from_bytes([3; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle.clone())?;

        Ok(ActiveWorkflow {
            store,
            visibility_store,
            loaded,
            runtime,
            supervision,
            registry,
            handle,
        })
    }

    fn context(active: &ActiveWorkflow) -> ContinueAsNewContext<'_> {
        ContinueAsNewContext {
            store: Arc::clone(&active.store),
            visibility_store: Arc::clone(&active.visibility_store),
            loaded_workflows: &active.loaded,
            runtime: &active.runtime,
            supervision: &active.supervision,
            registry: &active.registry,
        }
    }

    #[tokio::test]
    async fn pending_activity_rejects_without_terminal_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        {
            let recorder = active.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_activity_scheduled(
                    chrono::Utc::now(),
                    ActivityId::from_sequence_position(2),
                    "charge-card".to_owned(),
                    payload("activity")?,
                )
                .await?;
        }

        let result = continue_as_new(
            context(&active),
            active.handle.workflow_id(),
            active.handle.run_id(),
            ContinueAsNewRequest {
                input: payload("next")?,
                workflow_type: None,
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(EngineError::Runtime { reason }) if reason.contains("pending work")
        ));
        let history = active
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        assert!(!matches!(
            history.last(),
            Some(Event::WorkflowContinuedAsNew { .. })
        ));
        assert_eq!(
            active
                .registry
                .get(active.handle.workflow_id(), active.handle.run_id())?,
            Some(active.handle.clone())
        );
        active.runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn pending_child_rejects_without_terminal_event() -> Result<(), Box<dyn std::error::Error>>
    {
        let active = active_workflow().await?;
        {
            let recorder = active.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_child_workflow_started(
                    chrono::Utc::now(),
                    aion_core::WorkflowId::new_v4(),
                    "fulfillment".to_owned(),
                    payload("child")?,
                )
                .await?;
        }

        let result = continue_as_new(
            context(&active),
            active.handle.workflow_id(),
            active.handle.run_id(),
            ContinueAsNewRequest {
                input: payload("next")?,
                workflow_type: None,
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(EngineError::Runtime { reason }) if reason.contains("pending work")
        ));
        let history = active
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        assert!(!matches!(
            history.last(),
            Some(Event::WorkflowContinuedAsNew { .. })
        ));
        assert_eq!(
            active
                .registry
                .get(active.handle.workflow_id(), active.handle.run_id())?,
            Some(active.handle.clone())
        );
        active.runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn success_records_notifies_deregisters_and_starts_new_run()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        let old_workflow_id = active.handle.workflow_id().clone();
        let old_run_id = active.handle.run_id().clone();
        let input = payload("next")?;
        let mut receiver = active.handle.completion().subscribe();

        let new_handle = continue_as_new(
            context(&active),
            &old_workflow_id,
            &old_run_id,
            ContinueAsNewRequest {
                input: input.clone(),
                workflow_type: Some("checkout-v2".to_owned()),
            },
        )
        .await?;
        receiver.changed().await?;

        assert_eq!(new_handle.workflow_id(), &old_workflow_id);
        assert_ne!(new_handle.run_id(), &old_run_id);
        assert_eq!(new_handle.workflow_type(), "checkout-v2");
        assert_eq!(active.registry.get(&old_workflow_id, &old_run_id)?, None);
        assert_eq!(
            active.registry.get(&old_workflow_id, new_handle.run_id())?,
            Some(new_handle.clone())
        );
        assert_eq!(
            receiver.borrow().clone(),
            Some(TerminalOutcome::ContinuedAsNew {
                input: input.clone(),
                workflow_type: Some("checkout-v2".to_owned()),
                parent_run_id: old_run_id.clone(),
            })
        );

        let history = active.store.read_history(&old_workflow_id).await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowContinuedAsNew {
                    input: continued_input,
                    workflow_type,
                    parent_run_id,
                    ..
                },
                Event::WorkflowStarted {
                    input: started_input,
                    workflow_type: started_type,
                    parent_run_id: started_parent,
                    ..
                },
            ] => {
                assert_eq!(continued_input, &input);
                assert_eq!(workflow_type, &Some("checkout-v2".to_owned()));
                assert_eq!(parent_run_id, &old_run_id);
                assert_eq!(started_input, &input);
                assert_eq!(started_type, "checkout-v2");
                assert_eq!(started_parent, &Some(old_run_id));
            }
            other => {
                return Err(format!("expected continue-as-new history, found {other:?}").into());
            }
        }
        active.runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn unknown_replacement_type_rejects_before_terminal_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;

        let result = continue_as_new(
            context(&active),
            active.handle.workflow_id(),
            active.handle.run_id(),
            ContinueAsNewRequest {
                input: payload("next")?,
                workflow_type: Some("missing-workflow".to_owned()),
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(EngineError::WorkflowNotFound { workflow_type }) if workflow_type == "missing-workflow"
        ));
        let history = active
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        assert!(matches!(
            history.as_slice(),
            [Event::WorkflowStarted { workflow_type, .. }] if workflow_type == "checkout"
        ));
        assert_eq!(
            active
                .registry
                .get(active.handle.workflow_id(), active.handle.run_id())?,
            Some(active.handle.clone())
        );
        active.runtime.shutdown()?;
        Ok(())
    }
}
