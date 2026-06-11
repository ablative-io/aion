//! Continue-as-new lifecycle transition.

use std::collections::HashSet;
use std::sync::Arc;

use aion_core::{ActivityId, Event, Payload, RunId, SearchAttributeSchema, WorkflowId};
use aion_package::ContentHash;
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
    pub runtime: &'a Arc<RuntimeHandle>,
    /// Structural supervision tree recording the per-type supervisor placement.
    pub supervision: Arc<SupervisionTree>,
    /// Active execution registry keyed by workflow/run identifiers.
    pub registry: &'a Arc<Registry>,
    /// Schema validating initial search attributes on the replacement run.
    pub search_attribute_schema: Arc<SearchAttributeSchema>,
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

    let workflow_type = request
        .workflow_type
        .as_deref()
        .unwrap_or(handle.workflow_type());
    if workflow_type != handle.workflow_type() {
        return Err(EngineError::Runtime {
            reason: format!(
                "continue_as_new must restart the same workflow type: current={}, requested={workflow_type}",
                handle.workflow_type()
            ),
        });
    }
    validate_replacement_workflow_type(
        context.loaded_workflows,
        workflow_type,
        handle.loaded_version(),
    )?;

    {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        // History inspection and the terminal record are atomic under the
        // recorder lock: a concurrent cancel/complete/fail or freshly
        // resolving activity records through the same recorder, so checking
        // outside the lock would let a second terminal event (or a missed
        // pending-work item) slip in between check and record.
        let history = context.store.read_history(id).await?;
        if super::completion::terminal_outcome_from_history(&history, run).is_some() {
            return Err(EngineError::Runtime {
                reason: format!(
                    "continue_as_new rejected: workflow {id} run {run} already recorded a terminal event"
                ),
            });
        }
        guard_no_pending_work(&history)?;
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
            runtime: Arc::clone(context.runtime),
            supervision: context.supervision,
            registry: Arc::clone(context.registry),
            signal_handoff: None,
            search_attribute_schema: context.search_attribute_schema,
        },
        workflow_type,
        request.input.clone(),
        StartWorkflowOptions {
            workflow_id: Some(id.clone()),
            parent_run_id: Some(run.clone()),
            loaded_version: Some(handle.loaded_version().clone()),
            // Attributes already recorded in this workflow's history carry into
            // the replacement run's projection; nothing new is recorded here.
            search_attributes: std::collections::HashMap::new(),
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
            | Event::WithTimeoutCompleted { .. }
            | Event::SignalReceived { .. }
            | Event::SignalSent { .. }
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
    loaded_version: &ContentHash,
) -> Result<(), EngineError> {
    loaded_workflows
        .get(workflow_type, loaded_version)
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
        runtime: Arc<RuntimeHandle>,
        supervision: Arc<SupervisionTree>,
        registry: Arc<Registry>,
        handle: WorkflowHandle,
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn loaded_workflows() -> LoadedWorkflows {
        let mut loaded = LoadedWorkflows::new();
        loaded.note_loaded_workflow_for_test(
            "checkout",
            "checkout_deployed_v1",
            "run",
            ContentHash::from_bytes([3; 32]),
        );
        loaded.note_loaded_workflow_for_test(
            "checkout",
            "checkout_deployed_v2",
            "run",
            ContentHash::from_bytes([4; 32]),
        );
        loaded.note_loaded_workflow_for_test(
            "fulfillment",
            "fulfillment_deployed",
            "run",
            ContentHash::from_bytes([5; 32]),
        );
        loaded
    }

    async fn active_workflow() -> Result<ActiveWorkflow, Box<dyn std::error::Error>> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let loaded = loaded_workflows();
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        runtime.register_waiting_test_module("checkout_deployed_v1", "run");
        runtime.register_waiting_test_module("checkout_deployed_v2", "run");
        runtime.register_waiting_test_module("fulfillment_deployed", "run");
        let supervision = Arc::new(SupervisionTree::new());
        let registry = Arc::new(Registry::default());
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&store));
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                "checkout".to_owned(),
                payload("input")?,
                run_id.clone(),
            )
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
            supervision: Arc::clone(&active.supervision),
            registry: &active.registry,
            search_attribute_schema: Arc::new(aion_core::SearchAttributeSchema::new()),
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
                workflow_type: None,
            },
        )
        .await?;
        receiver.changed().await?;

        assert_eq!(new_handle.workflow_id(), &old_workflow_id);
        assert_ne!(new_handle.run_id(), &old_run_id);
        assert_eq!(new_handle.workflow_type(), "checkout");
        assert_eq!(
            new_handle.loaded_version(),
            &ContentHash::from_bytes([3; 32]),
            "replacement must use the old run's loaded version, not latest"
        );
        assert_eq!(active.registry.get(&old_workflow_id, &old_run_id)?, None);
        assert_eq!(
            active.registry.get(&old_workflow_id, new_handle.run_id())?,
            Some(new_handle.clone())
        );
        assert_eq!(
            receiver.borrow().clone(),
            Some(TerminalOutcome::ContinuedAsNew {
                input: input.clone(),
                workflow_type: None,
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
                    run_id: started_run_id,
                    parent_run_id: started_parent,
                    ..
                },
            ] => {
                assert_eq!(continued_input, &input);
                assert_eq!(workflow_type, &None);
                assert_eq!(parent_run_id, &old_run_id);
                assert_eq!(started_input, &input);
                assert_eq!(started_type, "checkout");
                assert_eq!(started_run_id, new_handle.run_id());
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
    async fn recorded_terminal_rejects_continue_without_second_terminal_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        {
            let recorder = active.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_cancelled(
                    chrono::Utc::now(),
                    "caller requested cancellation".to_owned(),
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
            Err(EngineError::Runtime { reason })
                if reason.contains("already recorded a terminal event")
        ));
        let history = active
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        assert!(matches!(
            history.as_slice(),
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowCancelled { .. }
            ]
        ));
        active.runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn different_replacement_type_rejects_before_terminal_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;

        let result = continue_as_new(
            context(&active),
            active.handle.workflow_id(),
            active.handle.run_id(),
            ContinueAsNewRequest {
                input: payload("next")?,
                workflow_type: Some("fulfillment".to_owned()),
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(EngineError::Runtime { reason })
                if reason.contains("must restart the same workflow type")
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
