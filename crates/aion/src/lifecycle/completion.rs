//! Process-exit completion handling.

use std::sync::Arc;

use aion_core::{Event, Payload, RunId, WorkflowError, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;
use tokio::runtime::Handle;

use crate::EngineError;
use crate::registry::{Registry, Residency, TerminalOutcome, WorkflowHandle};
use crate::runtime::WorkflowProcessOutcome;

use super::visibility::upsert_workflow_visibility;

/// Owned state needed by the runtime monitor callback.
#[derive(Clone)]
pub struct ProcessExitContext {
    /// Durable event store used to rebuild projections after terminal append.
    pub store: Arc<dyn EventStore>,
    /// Visibility index updated after terminal lifecycle events.
    pub visibility_store: Arc<dyn VisibilityStore>,
    /// Active execution registry to reconcile status and residency.
    pub registry: Arc<Registry>,
    /// Tokio runtime handle used to run async recorder/store work from the monitor thread.
    pub tokio_handle: Handle,
}

/// Handle one observed workflow process exit.
///
/// The monitor calls this from outside the workflow dirty NIF thread. All durable
/// terminal events are recorded through the handle-owned Recorder, then registry
/// projections are reconciled from authoritative history and subscribers are
/// notified.
///
/// # Errors
///
/// Returns typed recorder, store, visibility, or registry errors when completion
/// cannot be durably recorded or projected.
pub fn handle_process_exit(
    context: ProcessExitContext,
    handle: WorkflowHandle,
    outcome: Result<WorkflowProcessOutcome, EngineError>,
) -> Result<(), EngineError> {
    context
        .tokio_handle
        .clone()
        .block_on(handle_process_exit_async(context, handle, outcome))
}

async fn handle_process_exit_async(
    context: ProcessExitContext,
    handle: WorkflowHandle,
    outcome: Result<WorkflowProcessOutcome, EngineError>,
) -> Result<(), EngineError> {
    if let Some(existing) =
        terminal_outcome_from_history(&context.store.read_history(handle.workflow_id()).await?)
    {
        reconcile_terminal_registry(&context, handle.workflow_id(), handle.run_id()).await?;
        handle.completion().notify(existing);
        return Ok(());
    }

    let terminal = match outcome {
        Ok(WorkflowProcessOutcome::Completed(result)) => {
            record_completed(&handle, result.clone()).await?;
            TerminalOutcome::Completed(result)
        }
        Ok(WorkflowProcessOutcome::Failed(error)) => {
            record_failed(&handle, error.clone()).await?;
            TerminalOutcome::Failed(error)
        }
        Err(error) => {
            let workflow_error = WorkflowError {
                message: format!("workflow process monitor failed: {error}"),
                details: None,
            };
            record_failed(&handle, workflow_error.clone()).await?;
            TerminalOutcome::Failed(workflow_error)
        }
    };

    upsert_workflow_visibility(
        Arc::clone(&context.store),
        Arc::clone(&context.visibility_store),
        handle.workflow_id(),
        handle.run_id(),
    )
    .await?;
    reconcile_terminal_registry(&context, handle.workflow_id(), handle.run_id()).await?;
    handle.completion().notify(terminal);
    Ok(())
}

async fn record_completed(handle: &WorkflowHandle, result: Payload) -> Result<(), EngineError> {
    let recorder = handle.recorder();
    let mut recorder = recorder.lock().await;
    recorder
        .record_workflow_completed(Utc::now(), result)
        .await?;
    Ok(())
}

async fn record_failed(handle: &WorkflowHandle, error: WorkflowError) -> Result<(), EngineError> {
    let recorder = handle.recorder();
    let mut recorder = recorder.lock().await;
    recorder.record_workflow_failed(Utc::now(), error).await?;
    Ok(())
}

async fn reconcile_terminal_registry(
    context: &ProcessExitContext,
    id: &WorkflowId,
    run: &RunId,
) -> Result<(), EngineError> {
    let history = context.store.read_history(id).await?;
    context.registry.reconcile(id, run, &history)?;
    context
        .registry
        .replace_residency(id, run, Residency::Suspended)?;
    Ok(())
}

fn terminal_outcome_from_history(events: &[Event]) -> Option<TerminalOutcome> {
    for event in events.iter().rev() {
        match event {
            Event::WorkflowStarted { .. } => return None,
            Event::WorkflowCompleted { result, .. } => {
                return Some(TerminalOutcome::Completed(result.clone()));
            }
            Event::WorkflowFailed { error, .. } => {
                return Some(TerminalOutcome::Failed(error.clone()));
            }
            Event::WorkflowCancelled { reason, .. } => {
                return Some(TerminalOutcome::Cancelled(reason.clone()));
            }
            Event::WorkflowTimedOut { timeout, .. } => {
                return Some(TerminalOutcome::TimedOut(timeout.clone()));
            }
            Event::WorkflowContinuedAsNew {
                input,
                workflow_type,
                parent_run_id,
                ..
            } => {
                return Some(TerminalOutcome::ContinuedAsNew {
                    input: input.clone(),
                    workflow_type: workflow_type.clone(),
                    parent_run_id: parent_run_id.clone(),
                });
            }
            Event::SearchAttributesUpdated { .. }
            | Event::ActivityScheduled { .. }
            | Event::ActivityStarted { .. }
            | Event::ActivityCompleted { .. }
            | Event::ActivityFailed { .. }
            | Event::ActivityCancelled { .. }
            | Event::TimerStarted { .. }
            | Event::TimerFired { .. }
            | Event::TimerCancelled { .. }
            | Event::SignalReceived { .. }
            | Event::ChildWorkflowStarted { .. }
            | Event::ChildWorkflowCompleted { .. }
            | Event::ChildWorkflowFailed { .. }
            | Event::ChildWorkflowCancelled { .. }
            | Event::ScheduleCreated { .. }
            | Event::ScheduleUpdated { .. }
            | Event::SchedulePaused { .. }
            | Event::ScheduleResumed { .. }
            | Event::ScheduleDeleted { .. }
            | Event::ScheduleTriggered { .. } => {}
        }
    }
    None
}
