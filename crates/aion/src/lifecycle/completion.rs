//! Process-exit completion handling.

use std::sync::Arc;

use aion_core::{
    Event, Payload, RunId, WorkflowError, WorkflowId, current_lease_terminal, run_segment,
};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;
use tokio::runtime::Handle;

use crate::EngineError;
use crate::loader::WorkflowCatalog;
use crate::registry::{Registry, Residency, TerminalOutcome, WorkflowHandle};
use crate::runtime::{RuntimeHandle, WorkflowProcessOutcome};
use crate::supervision::SupervisionTree;

use super::start::{self, StartWorkflowContext, StartWorkflowOptions};
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
    /// Shared workflow catalog used to start continue-as-new replacements.
    pub catalog: Arc<WorkflowCatalog>,
    /// Runtime boundary used to spawn continue-as-new replacements.
    pub runtime: Arc<RuntimeHandle>,
    /// Structural supervision tree for replacement workflow placement.
    pub supervision: Arc<SupervisionTree>,
    /// Tokio runtime handle used to run async recorder/store work from the monitor thread.
    pub tokio_handle: Handle,
    /// Schema validating initial search attributes on continue-as-new replacements.
    pub search_attribute_schema: Arc<aion_core::SearchAttributeSchema>,
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
    // The terminal check and the terminal record must be atomic under the
    // recorder lock: a concurrent cancel/complete/fail transition records
    // through the same recorder, and a check outside the lock would let both
    // writers append a terminal event for the same run.
    let recorded = {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        let history = context.store.read_history(handle.workflow_id()).await?;
        if let Some(existing) = terminal_outcome_from_history(&history, handle.run_id()) {
            Err(existing)
        } else {
            match outcome {
                Ok(WorkflowProcessOutcome::Completed(result)) => {
                    recorder
                        .record_workflow_completed(Utc::now(), result.clone())
                        .await?;
                    Ok(TerminalOutcome::Completed(result))
                }
                Ok(WorkflowProcessOutcome::Failed(error)) => {
                    recorder
                        .record_workflow_failed(Utc::now(), error.clone())
                        .await?;
                    Ok(TerminalOutcome::Failed(error))
                }
                Err(error) => {
                    let workflow_error = WorkflowError {
                        message: format!("workflow process monitor failed: {error}"),
                        details: None,
                    };
                    recorder
                        .record_workflow_failed(Utc::now(), workflow_error.clone())
                        .await?;
                    Ok(TerminalOutcome::Failed(workflow_error))
                }
            }
        }
    };

    // Notify as soon as the durable terminal is decided: subscribers
    // (result waiters, child-terminal watchers) resolve from the recorded
    // store truth, so the doorbell must never be muted by a failure in the
    // post-record bookkeeping below — a watcher parked on a doorbell that
    // never rings strands the awaiting parent for the whole epoch.
    let terminal = match recorded {
        Err(existing) => {
            handle.completion().notify(existing.clone());
            reconcile_terminal_registry(&context, handle.workflow_id(), handle.run_id()).await?;
            if let TerminalOutcome::ContinuedAsNew {
                input,
                workflow_type,
                parent_run_id,
            } = existing
            {
                start_continuation_replacement(
                    &context,
                    &handle,
                    input,
                    workflow_type,
                    parent_run_id,
                )
                .await?;
            }
            return Ok(());
        }
        Ok(terminal) => terminal,
    };
    handle.completion().notify(terminal);

    upsert_workflow_visibility(
        Arc::clone(&context.store),
        Arc::clone(&context.visibility_store),
        handle.workflow_id(),
        handle.run_id(),
    )
    .await?;
    reconcile_terminal_registry(&context, handle.workflow_id(), handle.run_id()).await?;
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

async fn start_continuation_replacement(
    context: &ProcessExitContext,
    handle: &WorkflowHandle,
    input: Payload,
    workflow_type: Option<String>,
    parent_run_id: RunId,
) -> Result<(), EngineError> {
    let replacement_type = workflow_type.as_deref().unwrap_or(handle.workflow_type());
    let already_started = context
        .store
        .read_history(handle.workflow_id())
        .await?
        .iter()
        .any(|event| {
            matches!(
                event,
                Event::WorkflowStarted {
                    parent_run_id: Some(existing_parent),
                    ..
                } if existing_parent == &parent_run_id
            )
        });
    if already_started {
        return Ok(());
    }

    start::start_workflow_with_options(
        StartWorkflowContext {
            store: Arc::clone(&context.store),
            visibility_store: Arc::clone(&context.visibility_store),
            catalog: Arc::clone(&context.catalog),
            runtime: Arc::clone(&context.runtime),
            supervision: Arc::clone(&context.supervision),
            registry: Arc::clone(&context.registry),
            signal_handoff: None,
            search_attribute_schema: Arc::clone(&context.search_attribute_schema),
            monitor_tokio_handle: context.tokio_handle.clone(),
        },
        replacement_type,
        input,
        StartWorkflowOptions {
            workflow_id: Some(handle.workflow_id().clone()),
            // Continue-as-new reuses the existing id; steering does not apply.
            routing_key: None,
            parent_run_id: Some(parent_run_id),
            // D1: the continue-as-new successor resolves the latest loaded
            // version at record time, identically to the startup sweep.
            loaded_version: None,
            // Recorded attributes carry into the replacement run's projection.
            search_attributes: std::collections::HashMap::new(),
            namespace: Some(handle.namespace().to_owned()),
        },
    )
    .await?;
    Ok(())
}

pub(crate) fn terminal_outcome_from_history(
    events: &[Event],
    run_id: &RunId,
) -> Option<TerminalOutcome> {
    // Reset-aware: scope to the run, then take the current lease's terminal
    // event. A WorkflowReopened after a terminal supersedes it, so a reopened run
    // reports no terminal outcome until it terminates again.
    match current_lease_terminal(run_segment(events, run_id))? {
        Event::WorkflowCompleted { result, .. } => Some(TerminalOutcome::Completed(result.clone())),
        Event::WorkflowFailed { error, .. } => Some(TerminalOutcome::Failed(error.clone())),
        Event::WorkflowCancelled { reason, .. } => Some(TerminalOutcome::Cancelled(reason.clone())),
        Event::WorkflowTimedOut { timeout, .. } => Some(TerminalOutcome::TimedOut(timeout.clone())),
        Event::WorkflowContinuedAsNew {
            input,
            workflow_type,
            parent_run_id,
            ..
        } if parent_run_id == run_id => Some(TerminalOutcome::ContinuedAsNew {
            input: input.clone(),
            workflow_type: workflow_type.clone(),
            parent_run_id: parent_run_id.clone(),
        }),
        // current_lease_terminal yields only terminal lifecycle events; a
        // ContinuedAsNew whose parent is a different run is not this run's
        // outcome.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::{ProcessExitContext, handle_process_exit_async, terminal_outcome_from_history};
    use crate::durability::Recorder;
    use crate::loader::WorkflowCatalog;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, TerminalOutcome, WorkflowHandle,
        WorkflowHandleParts,
    };
    use crate::runtime::{RuntimeConfig, RuntimeHandle, WorkflowProcessOutcome};
    use crate::supervision::SupervisionTree;

    struct ActiveWorkflow {
        context: ProcessExitContext,
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
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let registry = Arc::new(Registry::default());
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&store));
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                crate::durability::WorkflowStartRecord {
                    workflow_type: "checkout".to_owned(),
                    input: payload("input")?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid: 1,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: ContentHash::from_bytes([9; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle.clone())?;
        Ok(ActiveWorkflow {
            context: ProcessExitContext {
                store,
                visibility_store,
                registry,
                catalog: Arc::new(WorkflowCatalog::new()),
                runtime: Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?),
                supervision: Arc::new(SupervisionTree::new()),
                tokio_handle: tokio::runtime::Handle::current(),
                search_attribute_schema: Arc::new(aion_core::SearchAttributeSchema::new()),
            },
            handle,
        })
    }

    #[tokio::test]
    async fn normal_exit_records_completed_reconciles_and_notifies()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        let result = payload("result")?;
        let mut early = active.handle.completion().subscribe();

        handle_process_exit_async(
            active.context.clone(),
            active.handle.clone(),
            Ok(WorkflowProcessOutcome::Completed(result.clone())),
        )
        .await?;
        early.changed().await?;

        assert_eq!(
            early.borrow().clone(),
            Some(TerminalOutcome::Completed(result.clone()))
        );
        assert_eq!(
            active.handle.completion().subscribe().borrow().clone(),
            Some(TerminalOutcome::Completed(result.clone()))
        );
        let registered = active
            .context
            .registry
            .get(active.handle.workflow_id(), active.handle.run_id())?
            .ok_or("missing registered handle")?;
        assert_eq!(registered.cached_status(), WorkflowStatus::Completed);
        assert_eq!(registered.residency(), HandleResidency::Suspended);
        let history = active
            .context
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowCompleted {
                    result: recorded, ..
                },
            ] => {
                assert_eq!(recorded, &result);
            }
            other => return Err(format!("expected started then completed, found {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn terminal_outcome_is_scoped_to_requested_run_segment()
    -> Result<(), Box<dyn std::error::Error>> {
        let old_run_id = aion_core::RunId::new(uuid::Uuid::from_u128(1));
        let new_run_id = aion_core::RunId::new(uuid::Uuid::from_u128(2));
        let input = payload("next")?;
        let result = payload("done")?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let envelope = |seq| aion_core::EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        };
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1),
                workflow_type: "checkout".to_owned(),
                input: payload("first")?,
                run_id: old_run_id.clone(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2),
                input: input.clone(),
                workflow_type: None,
                parent_run_id: old_run_id.clone(),
            },
            Event::WorkflowStarted {
                envelope: envelope(3),
                workflow_type: "checkout".to_owned(),
                input,
                run_id: new_run_id.clone(),
                parent_run_id: Some(old_run_id.clone()),
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowCompleted {
                envelope: envelope(4),
                result: result.clone(),
            },
        ];

        assert_eq!(
            terminal_outcome_from_history(&events, &old_run_id),
            Some(TerminalOutcome::ContinuedAsNew {
                input: payload("next")?,
                workflow_type: None,
                parent_run_id: old_run_id,
            })
        );
        assert_eq!(
            terminal_outcome_from_history(&events, &new_run_id),
            Some(TerminalOutcome::Completed(result))
        );
        Ok(())
    }

    #[tokio::test]
    async fn abnormal_exit_records_failed_reconciles_and_notifies()
    -> Result<(), Box<dyn std::error::Error>> {
        let active = active_workflow().await?;
        let error = workflow_error("process crashed: error");
        let mut early = active.handle.completion().subscribe();

        handle_process_exit_async(
            active.context.clone(),
            active.handle.clone(),
            Ok(WorkflowProcessOutcome::Failed(error.clone())),
        )
        .await?;
        early.changed().await?;

        assert_eq!(
            early.borrow().clone(),
            Some(TerminalOutcome::Failed(error.clone()))
        );
        assert_eq!(
            active.handle.completion().subscribe().borrow().clone(),
            Some(TerminalOutcome::Failed(error.clone()))
        );
        let registered = active
            .context
            .registry
            .get(active.handle.workflow_id(), active.handle.run_id())?
            .ok_or("missing registered handle")?;
        assert_eq!(registered.cached_status(), WorkflowStatus::Failed);
        assert_eq!(registered.residency(), HandleResidency::Suspended);
        let history = active
            .context
            .store
            .read_history(active.handle.workflow_id())
            .await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowFailed {
                    error: recorded, ..
                },
            ] => {
                assert_eq!(recorded, &error);
            }
            other => return Err(format!("expected started then failed, found {other:?}").into()),
        }
        Ok(())
    }
}
