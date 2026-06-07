//! cancel/complete/fail transitions

use aion_core::{Payload, RunId, WorkflowError, WorkflowId};
use chrono::Utc;

use crate::EngineError;
use crate::registry::{Registry, TerminalOutcome, WorkflowHandle};
use crate::runtime::RuntimeHandle;

/// Dependencies required to drive a workflow to a terminal lifecycle state.
pub struct TerminateWorkflowContext<'a> {
    /// Runtime boundary used to cancel live workflow processes.
    pub runtime: &'a RuntimeHandle,
    /// Active execution registry keyed by workflow/run identifiers.
    pub registry: &'a Registry,
}

/// Completes a live workflow run with its terminal result payload.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair is
/// not registered. Recorder and registry failures surface as their typed
/// [`EngineError`] variants.
pub async fn complete(
    context: TerminateWorkflowContext<'_>,
    id: &WorkflowId,
    run: &RunId,
    result: Payload,
) -> Result<(), EngineError> {
    let handle = registered_handle(context.registry, id, run)?;
    {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        recorder
            .record_workflow_completed(Utc::now(), result.clone())
            .await?;
    }

    handle
        .completion()
        .notify(TerminalOutcome::Completed(result));
    context.registry.remove(id, run)?;
    Ok(())
}

/// Fails a live workflow run with its terminal workflow error.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair is
/// not registered. Recorder and registry failures surface as their typed
/// [`EngineError`] variants.
pub async fn fail(
    context: TerminateWorkflowContext<'_>,
    id: &WorkflowId,
    run: &RunId,
    error: WorkflowError,
) -> Result<(), EngineError> {
    let handle = registered_handle(context.registry, id, run)?;
    {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        recorder
            .record_workflow_failed(Utc::now(), error.clone())
            .await?;
    }

    handle.completion().notify(TerminalOutcome::Failed(error));
    context.registry.remove(id, run)?;
    Ok(())
}

/// Cancels a live workflow run, relying on runtime link propagation to tear down
/// any linked activity children.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair is
/// not registered. Runtime cancellation, recorder, and registry failures surface
/// as their typed [`EngineError`] variants.
pub async fn cancel(
    context: TerminateWorkflowContext<'_>,
    id: &WorkflowId,
    run: &RunId,
    reason: impl Into<String>,
) -> Result<(), EngineError> {
    let handle = registered_handle(context.registry, id, run)?;
    context.runtime.cancel_pid(handle.pid())?;

    let reason = reason.into();
    {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        recorder
            .record_workflow_cancelled(Utc::now(), reason.clone())
            .await?;
    }

    handle
        .completion()
        .notify(TerminalOutcome::Cancelled(reason));
    context.registry.remove(id, run)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::{TerminateWorkflowContext, cancel, complete, fail};
    use crate::EngineError;
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, TerminalOutcome, WorkflowHandle,
        WorkflowHandleParts,
    };
    use crate::runtime::{RuntimeConfig, RuntimeHandle};

    struct ActiveWorkflow {
        store: Arc<dyn EventStore>,
        runtime: RuntimeHandle,
        registry: Registry,
        handle: WorkflowHandle,
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn workflow_error(message: &str) -> aion_core::WorkflowError {
        aion_core::WorkflowError {
            message: message.to_owned(),
            details: None,
        }
    }

    async fn active_workflow() -> Result<ActiveWorkflow, Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&store));
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                "checkout".to_owned(),
                payload("input")?,
                aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            )
            .await?;
        let pid = runtime.spawn_test_process_with_trap_exit(true)?;
        let completion = CompletionNotifier::new();
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "checkout".to_owned(),
            loaded_version: ContentHash::from_bytes([9; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion,
        });
        registry.insert((workflow_id, run_id), handle.clone())?;

        Ok(ActiveWorkflow {
            store,
            runtime,
            registry,
            handle,
        })
    }

    fn context<'a>(
        runtime: &'a RuntimeHandle,
        registry: &'a Registry,
    ) -> TerminateWorkflowContext<'a> {
        TerminateWorkflowContext { runtime, registry }
    }

    #[tokio::test]
    async fn complete_records_notifies_and_deregisters() -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        let result = payload("result")?;
        let mut receiver = active.handle.completion().subscribe();

        complete(
            context(&active.runtime, &active.registry),
            active.handle.workflow_id(),
            active.handle.run_id(),
            result.clone(),
        )
        .await?;
        receiver.changed().await?;

        assert_eq!(
            receiver.borrow().clone(),
            Some(TerminalOutcome::Completed(result.clone()))
        );
        assert_eq!(
            active
                .registry
                .get(active.handle.workflow_id(), active.handle.run_id())?,
            None
        );
        let history = active
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowCompleted {
                    envelope,
                    result: recorded,
                },
            ] => {
                assert_eq!(envelope.seq, 2);
                assert_eq!(recorded, &result);
            }
            other => return Err(format!("expected started then completed, found {other:?}").into()),
        }
        active.runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn fail_records_notifies_and_deregisters() -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        let error = workflow_error("workflow failed");
        let mut receiver = active.handle.completion().subscribe();

        fail(
            context(&active.runtime, &active.registry),
            active.handle.workflow_id(),
            active.handle.run_id(),
            error.clone(),
        )
        .await?;
        receiver.changed().await?;

        assert_eq!(
            receiver.borrow().clone(),
            Some(TerminalOutcome::Failed(error.clone()))
        );
        assert_eq!(
            active
                .registry
                .get(active.handle.workflow_id(), active.handle.run_id())?,
            None
        );
        let history = active
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowFailed {
                    envelope,
                    error: recorded,
                },
            ] => {
                assert_eq!(envelope.seq, 2);
                assert_eq!(recorded, &error);
            }
            other => return Err(format!("expected started then failed, found {other:?}").into()),
        }
        active.runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_kills_linked_children_records_notifies_and_deregisters()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        let child = active
            .runtime
            .spawn_linked_test_process(active.handle.pid())?;
        let reason = String::from("caller requested cancellation");
        let mut receiver = active.handle.completion().subscribe();

        cancel(
            context(&active.runtime, &active.registry),
            active.handle.workflow_id(),
            active.handle.run_id(),
            reason.clone(),
        )
        .await?;
        receiver.changed().await?;

        assert!(!active.runtime.is_live(active.handle.pid()));
        assert!(!active.runtime.is_live(child));
        assert_eq!(
            receiver.borrow().clone(),
            Some(TerminalOutcome::Cancelled(reason.clone()))
        );
        assert_eq!(
            active
                .registry
                .get(active.handle.workflow_id(), active.handle.run_id())?,
            None
        );
        let history = active
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowCancelled {
                    envelope,
                    reason: recorded,
                },
            ] => {
                assert_eq!(envelope.seq, 2);
                assert_eq!(recorded, &reason);
            }
            other => return Err(format!("expected started then cancelled, found {other:?}").into()),
        }
        active.runtime.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_unknown_workflow_returns_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();

        let result = cancel(
            context(&runtime, &registry),
            &workflow_id,
            &run_id,
            "missing workflow",
        )
        .await;

        assert!(matches!(result, Err(EngineError::WorkflowNotFound { .. })));
        runtime.shutdown()?;
        Ok(())
    }
}
