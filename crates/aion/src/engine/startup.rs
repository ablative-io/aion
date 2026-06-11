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
    schedule_coordinator_package_version, schedule_coordinator_run_id,
    schedule_coordinator_workflow_id, schedule_coordinator_workflow_type,
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
    context: &StartupRecoveryContext<'_>,
    recovery: &dyn ActiveWorkflowRecoverySeam,
) -> Result<(), EngineError> {
    let store = &context.store;
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

        // Per-workflow isolation (#62): a run whose pinned package version
        // (or replay metadata) cannot be resolved fails its own recovery with
        // a typed error, logged here; it must not abort the engine build or
        // other workflows' recovery.
        let recovered = match recover_active_workflow(
            recovery,
            &workflow_id,
            &workflow_type,
            &history,
            loaded_workflows,
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
    context: &StartupRecoveryContext<'_>,
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
                    loaded_workflows: Arc::new(context.loaded_workflows.clone()),
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
                loaded_workflows: context.loaded_workflows,
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
        let started = start_workflow_with_options(
            StartWorkflowContext {
                store: Arc::clone(&context.store),
                visibility_store: Arc::clone(&context.visibility_store),
                loaded_workflows: context.loaded_workflows,
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

/// Whether a successor `WorkflowStarted` continuing `continued_run_id` is
/// durable for `workflow_id`.
async fn successor_started(
    context: &StartupRecoveryContext<'_>,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use aion_core::{
        Event, EventEnvelope, Payload, RunId, SearchAttributeSchema, WorkflowId, WorkflowStatus,
    };
    use aion_store::{EventStore, StoreError};
    use chrono::Utc;
    use serde_json::json;

    use super::{StartupRecoveryContext, sweep_continued_as_new_replacements};
    use crate::EngineError;
    use crate::loader::LoadedWorkflows;
    use crate::registry::Registry;
    use crate::runtime::{RuntimeConfig, RuntimeHandle};
    use crate::supervision::SupervisionTree;
    use aion_store::InMemoryStore;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Canned store modelling the N-5 race: the first history read shows a
    /// stranded continue-as-new run (no successor `WorkflowStarted`); once
    /// `successor_appears_after_reads` reads have happened, the racing exit
    /// monitor's successor start is visible. Appends are never expected —
    /// the sweep's own start fails earlier (no loadable type), standing in
    /// for the race loser's `SequenceConflict`.
    struct RacingSuccessorStore {
        workflow_id: WorkflowId,
        base_history: Vec<Event>,
        full_history: Vec<Event>,
        successor_appears_after_reads: u32,
        reads: AtomicU32,
        appears: bool,
    }

    #[async_trait::async_trait]
    impl aion_store::ReadableEventStore for RacingSuccessorStore {
        async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
            if workflow_id != &self.workflow_id {
                return Ok(Vec::new());
            }
            let read = self.reads.fetch_add(1, Ordering::AcqRel) + 1;
            if self.appears && read > self.successor_appears_after_reads {
                Ok(self.full_history.clone())
            } else {
                Ok(self.base_history.clone())
            }
        }

        async fn read_history_from(
            &self,
            workflow_id: &WorkflowId,
            from_seq: u64,
        ) -> Result<Vec<Event>, StoreError> {
            let _ = (workflow_id, from_seq);
            Err(StoreError::Backend(
                "unexpected read_history_from in the sweep test".to_owned(),
            ))
        }

        async fn read_run_chain(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<aion_store::RunSummary>, StoreError> {
            let _ = workflow_id;
            Err(StoreError::Backend(
                "unexpected read_run_chain in the sweep test".to_owned(),
            ))
        }

        async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
            Ok(vec![self.workflow_id.clone()])
        }

        async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
            Ok(Vec::new())
        }

        async fn query(
            &self,
            filter: &aion_core::WorkflowFilter,
        ) -> Result<Vec<aion_core::WorkflowSummary>, StoreError> {
            if filter.status != Some(WorkflowStatus::ContinuedAsNew) {
                return Ok(Vec::new());
            }
            Ok(vec![aion_core::WorkflowSummary {
                workflow_id: self.workflow_id.clone(),
                workflow_type: "checkout".to_owned(),
                status: WorkflowStatus::ContinuedAsNew,
                started_at: Utc::now(),
                ended_at: None,
                parent: None,
            }])
        }

        async fn schedule_timer(
            &self,
            workflow_id: &WorkflowId,
            timer_id: &aion_core::TimerId,
            fire_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), StoreError> {
            let _ = (workflow_id, timer_id, fire_at);
            Err(StoreError::Backend(
                "unexpected schedule_timer in the sweep test".to_owned(),
            ))
        }

        async fn expired_timers(
            &self,
            as_of: chrono::DateTime<chrono::Utc>,
        ) -> Result<Vec<aion_store::TimerEntry>, StoreError> {
            let _ = as_of;
            Ok(Vec::new())
        }
    }

    #[async_trait::async_trait]
    impl aion_store::WritableEventStore for RacingSuccessorStore {
        async fn append(
            &self,
            token: aion_store::WriteToken,
            workflow_id: &WorkflowId,
            events: &[Event],
            expected_seq: u64,
        ) -> Result<(), StoreError> {
            let _ = (token, workflow_id, events, expected_seq);
            Err(StoreError::SequenceConflict {
                expected: expected_seq,
                found: expected_seq + 1,
            })
        }
    }

    /// `(base history, history with the successor, continued run id)`.
    type StrandedHistories = (Vec<Event>, Vec<Event>, RunId);

    fn stranded_histories(
        workflow_id: &WorkflowId,
    ) -> Result<StrandedHistories, Box<dyn std::error::Error>> {
        let first_run = RunId::new_v4();
        let second_run = RunId::new_v4();
        let envelope = |seq: u64| EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id.clone(),
        };
        let input = Payload::from_json(&json!({"next": true}))?;
        let base = vec![
            Event::WorkflowStarted {
                envelope: envelope(1),
                workflow_type: "checkout".to_owned(),
                input: Payload::from_json(&json!({"first": true}))?,
                run_id: first_run.clone(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2),
                input: input.clone(),
                workflow_type: None,
                parent_run_id: first_run.clone(),
            },
        ];
        let mut full = base.clone();
        full.push(Event::WorkflowStarted {
            envelope: envelope(3),
            workflow_type: "checkout".to_owned(),
            input,
            run_id: second_run,
            parent_run_id: Some(first_run.clone()),
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        });
        Ok((base, full, first_run))
    }

    fn recovery_context(
        store: Arc<dyn EventStore>,
        runtime: Arc<RuntimeHandle>,
        loaded_workflows: &LoadedWorkflows,
    ) -> StartupRecoveryContext<'_> {
        StartupRecoveryContext {
            store,
            visibility_store: Arc::new(InMemoryStore::default()),
            runtime,
            loaded_workflows,
            registry: Arc::new(Registry::default()),
            supervision: Arc::new(SupervisionTree::new()),
            recovery: None,
            search_attribute_schema: Arc::new(SearchAttributeSchema::new()),
        }
    }

    /// N-5: the sweep's start races the recovered run's exit monitor, which
    /// starts the same successor concurrently. When the sweep's start fails
    /// but a re-read shows the successor `WorkflowStarted` durable (the
    /// winner's append), the failure is benign and `EngineBuilder::build`
    /// must not fail. Before the fix the sweep propagated the loser's error
    /// and the whole build failed on a `SequenceConflict`-class race.
    #[tokio::test(flavor = "multi_thread")]
    async fn sweep_start_race_lost_to_the_exit_monitor_is_benign() -> TestResult {
        let workflow_id = WorkflowId::new_v4();
        let (base, full, _continued) = stranded_histories(&workflow_id)?;
        let store = Arc::new(RacingSuccessorStore {
            workflow_id,
            base_history: base,
            full_history: full,
            // Read #1 is the sweep's pre-start read (no successor yet); the
            // racing monitor wins during the sweep's failed start, so the
            // post-failure re-read (#2) sees the successor.
            successor_appears_after_reads: 1,
            reads: AtomicU32::new(0),
            appears: true,
        });
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let loaded = LoadedWorkflows::new();
        let context = recovery_context(store as Arc<dyn EventStore>, Arc::clone(&runtime), &loaded);

        sweep_continued_as_new_replacements(&context)
            .await
            .map_err(|error| format!("a lost start race must be benign (N-5): {error}"))?;
        runtime.shutdown()?;
        Ok(())
    }

    /// The guard must not swallow real failures: a start failure with NO
    /// durable successor is a genuine fault and still fails the build.
    #[tokio::test(flavor = "multi_thread")]
    async fn sweep_start_failure_without_a_successor_still_fails() -> TestResult {
        let workflow_id = WorkflowId::new_v4();
        let (base, full, _continued) = stranded_histories(&workflow_id)?;
        let store = Arc::new(RacingSuccessorStore {
            workflow_id,
            base_history: base,
            full_history: full,
            successor_appears_after_reads: u32::MAX,
            reads: AtomicU32::new(0),
            appears: false,
        });
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let loaded = LoadedWorkflows::new();
        let context = recovery_context(store as Arc<dyn EventStore>, Arc::clone(&runtime), &loaded);

        let result = sweep_continued_as_new_replacements(&context).await;
        assert!(
            matches!(result, Err(EngineError::WorkflowNotFound { .. })),
            "a start failure without a durable successor must propagate: {result:?}"
        );
        runtime.shutdown()?;
        Ok(())
    }
}
