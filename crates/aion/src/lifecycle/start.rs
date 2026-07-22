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
use crate::loader::WorkflowCatalog;
use crate::registry::{
    CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
};
use crate::runtime::monitor::UnmonitoredProcessAbortError;
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
pub struct StartWorkflowContext {
    /// Durable event store used by the workflow's single recorder.
    pub store: Arc<dyn EventStore>,
    /// Visibility index updated after state-changing workflow events.
    pub visibility_store: Arc<dyn VisibilityStore>,
    /// Shared workflow catalog resolving types to loaded package versions.
    pub catalog: Arc<WorkflowCatalog>,
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
    /// Caller-chosen R-4 steered-start routing key. When set (and no explicit
    /// `workflow_id` is supplied), the request-routing edge derives a fresh id on
    /// `shard_for(routing_key)` so the start is *steered* to that shard's owner —
    /// forwarded there when this node is not the owner. `None` (the default) keeps
    /// the unsteered R-1 remint behaviour, so the single-node path is unchanged.
    pub routing_key: Option<String>,
    /// Parent run that continued into this run, when applicable.
    pub parent_run_id: Option<RunId>,
    /// Exact loaded package version to spawn; omitted to use the latest version.
    pub loaded_version: Option<ContentHash>,
    /// Initial search attributes recorded atomically with `WorkflowStarted`.
    pub search_attributes: HashMap<String, SearchAttributeValue>,
    /// Namespace that owns this workflow execution; defaults to `"default"`.
    pub namespace: Option<String>,
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
    context: StartWorkflowContext,
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
    context: StartWorkflowContext,
    workflow_type: &str,
    input: Payload,
    options: StartWorkflowOptions,
) -> Result<WorkflowHandle, EngineError> {
    // The pinned resolution is held for the whole start: from here until the
    // registry insert lands (or the start fails), unload verification sees
    // this version as in use, closing the registration birth window.
    let pinned = match &options.loaded_version {
        Some(version) => context.catalog.resolve_exact(workflow_type, version)?,
        None => context.catalog.resolve_routed(workflow_type)?,
    }
    .ok_or_else(|| EngineError::WorkflowNotFound {
        workflow_type: workflow_type.to_owned(),
    })?;
    let loaded = pinned.workflow();

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
    // Single deterministic clock read: `WorkflowStarted.recorded_at` AND the
    // deadline's `fire_at` both derive from it, so a replay/adoption re-arm
    // computes the identical fire time (never a second live-clock read).
    let started_at = Utc::now();
    let mut recorder = Recorder::resume_at(
        workflow_id.clone(),
        Arc::clone(&context.store),
        initial_head,
    )
    .with_visibility(run_id.clone(), Arc::clone(&context.visibility_store));
    recorder
        .record_workflow_started_with_attributes(
            started_at,
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
    let armed_deadline = record_declared_deadline(
        &mut recorder,
        &run_id,
        loaded.declared_timeout(),
        started_at,
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
        return Err(abort_unmonitored_start(&context.runtime, pid, error));
    }

    let completion = CompletionNotifier::new();
    let handle = WorkflowHandle::new(WorkflowHandleParts {
        workflow_id: workflow_id.clone(),
        run_id: run_id.clone(),
        pid,
        workflow_type: loaded.workflow_type().to_owned(),
        namespace: options.namespace.unwrap_or_else(|| String::from("default")),
        loaded_version: loaded.version().clone(),
        cached_status: WorkflowStatus::Running,
        residency: HandleResidency::Resident,
        recorder,
        completion,
    });

    publish_started_handle(&context, &handle)?;

    arm_declared_deadline_or_fail_start(&context, &workflow_id, &handle, armed_deadline).await?;

    install_started_monitor(&context, &handle)?;
    deliver_deferred_signals(&context, &handle);

    Ok(handle)
}

/// Establish the declared deadline LIVE before acknowledging the start, failing
/// the start honestly if it cannot be armed.
///
/// The durable timer row and the live wheel are armed here (after registration,
/// so the workflow is resident and the wheel actually arms), while the recorded
/// `TimerStarted` is the recovery anchor. A declared deadline MUST arm — a run
/// that proceeded with a silently-inert deadline would run forever if this single
/// attempt failed and the engine stayed alive — so a persistence or arming
/// failure retracts the registry publication and aborts the still-unmonitored
/// process, returning the typed error with no half-started run. A run with no
/// declared deadline (`armed_deadline == None`) is a no-op.
///
/// # Errors
///
/// Returns the typed [`EngineError`] from [`arm_declared_deadline`] after
/// retracting the partial start.
async fn arm_declared_deadline_or_fail_start(
    context: &StartWorkflowContext,
    workflow_id: &WorkflowId,
    handle: &WorkflowHandle,
    armed_deadline: Option<(aion_core::TimerId, chrono::DateTime<Utc>)>,
) -> Result<(), EngineError> {
    let Some((deadline_id, fire_at)) = armed_deadline else {
        return Ok(());
    };
    if let Err(error) = arm_declared_deadline(context, workflow_id, deadline_id, fire_at).await {
        return Err(fail_started_run(context, handle, error));
    }
    Ok(())
}

/// Fails a partially-started run without orphaning its still-unmonitored process.
///
/// Ordering is the invariant: the registry publication (ownership) is retracted
/// ONLY after the process has been terminated and that termination synchronously
/// observed by [`RuntimeHandle::abort_unmonitored_process`]. If termination
/// cannot be guaranteed — the bounded cleanup queue is
/// `Unavailable`/`Exhausted`/`Poisoned` — the publication is RETAINED so the live
/// process stays owned rather than becoming an unowned, unmonitored orphan, and a
/// completion monitor is installed so its eventual exit is reconciled and cleanup
/// stays retryable. The original `cause` is surfaced either way (a retain-path
/// abort failure is logged at error level).
fn fail_started_run(
    context: &StartWorkflowContext,
    handle: &WorkflowHandle,
    cause: EngineError,
) -> EngineError {
    let pid = handle.pid();
    match context.runtime.abort_unmonitored_process(pid) {
        Ok(()) => {
            // Terminated and synchronously observed; retracting cannot orphan.
            retract_registry_publication(context, handle);
        }
        Err(UnmonitoredProcessAbortError::CleanupFailed { reason, .. }) => {
            // The abort DID terminate the process — `terminate_process` runs
            // before the ancillary cleanup that failed — so the retained
            // publication is now stale: retract it. The ancillary cleanup failure
            // is the runtime's own to retry; surface it loudly.
            tracing::error!(
                workflow_pid = pid,
                %reason,
                cause = %cause,
                "failed-start abort terminated the process but ancillary cleanup failed; retracting stale ownership"
            );
            retract_registry_publication(context, handle);
        }
        Err(UnmonitoredProcessAbortError::TimedOut { .. }) => {
            // The abort job is still in flight and WILL terminate the process.
            // Defer retraction to the job's completion finalizer rather than
            // racing a monitor install the in-flight job would reject.
            defer_retraction_to_abort_job(context, handle, pid);
        }
        Err(
            error @ (UnmonitoredProcessAbortError::ExecutorUnavailable { .. }
            | UnmonitoredProcessAbortError::ExecutorExhausted { .. }
            | UnmonitoredProcessAbortError::ExecutorPoisoned { .. }),
        ) => {
            // No abort job was submitted (the reservation was released) and the
            // process is still live and unmonitored: retain ownership and install
            // a completion monitor so its eventual exit is reconciled.
            tracing::error!(
                workflow_pid = pid,
                %error,
                cause = %cause,
                "failed-start abort could not be submitted; retaining ownership and installing a completion monitor"
            );
            retain_ownership_with_monitor(context, handle);
        }
        Err(error) => {
            // Poisoned/degraded abort state: termination cannot be proven, so
            // retain ownership rather than orphan. The typed cause already reaches
            // the caller.
            tracing::error!(
                workflow_pid = pid,
                %error,
                cause = %cause,
                "failed-start abort returned a degraded state; retaining ownership as the safe cleanup backstop"
            );
        }
    }
    cause
}

/// Defer failed-start registry retraction to the in-flight abort job's
/// completion: when the job proves termination it runs the retraction finalizer.
/// If no job is found the abort completed between the wait timeout and here, so
/// the process is already terminated and retraction runs now. A poisoned attach
/// retains ownership (the safe backstop).
fn defer_retraction_to_abort_job(
    context: &StartWorkflowContext,
    handle: &WorkflowHandle,
    pid: crate::Pid,
) {
    let registry = Arc::clone(&context.registry);
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();
    let finalizer = move || {
        if let Err(error) = registry.remove(&workflow_id, &run_id) {
            tracing::warn!(
                workflow_pid = pid,
                %error,
                "failed to retract failed-start ownership after its abort job terminated the process"
            );
        }
    };
    match context
        .runtime
        .attach_unmonitored_abort_finalizer(pid, finalizer)
    {
        Ok(true) => tracing::warn!(
            workflow_pid = pid,
            "failed-start abort timed out; registry retraction deferred to the abort job's termination"
        ),
        Ok(false) => retract_registry_publication(context, handle),
        Err(error) => tracing::error!(
            workflow_pid = pid,
            %error,
            "could not attach failed-start retraction to the abort job; retaining ownership"
        ),
    }
}

/// Keeps a failed-start run owned when its process could not be aborted:
/// installs a completion monitor so the eventual exit is reconciled. A failed
/// installation is logged — the retained registry publication remains the
/// cleanup backstop, so ownership is never dropped on the floor.
fn retain_ownership_with_monitor(context: &StartWorkflowContext, handle: &WorkflowHandle) {
    if let Err(error) = install_completion_monitor(context, handle.pid(), handle) {
        tracing::error!(
            workflow_pid = handle.pid(),
            error = %error,
            "failed to install a completion monitor for a retained failed-start run; registry ownership remains the cleanup backstop"
        );
    }
}

/// Record the run's declared-timeout deadline `TimerStarted`, if one is declared.
///
/// LAW 1: a `None` `declared_timeout` records nothing — no `TimerStarted`, no
/// durable row, no deadline object of any kind — and returns `None`. When a
/// timeout is declared, this records the durable anchor (`timer_is_live` and
/// `outstanding_future_timers` recover the deadline from it after
/// failover/adoption) and returns the id and `fire_at` for the caller to arm the
/// live wheel after registration.
///
/// # Errors
///
/// Returns [`EngineError`] when the fire time is out of range, the deadline id
/// cannot be minted, or the `TimerStarted` append fails.
async fn record_declared_deadline(
    recorder: &mut Recorder,
    run_id: &RunId,
    declared_timeout: Option<std::time::Duration>,
    started_at: chrono::DateTime<Utc>,
) -> Result<Option<(aion_core::TimerId, chrono::DateTime<Utc>)>, EngineError> {
    let Some(timeout) = declared_timeout else {
        return Ok(None);
    };
    let fire_at = deadline_fire_at(started_at, timeout)?;
    let deadline_id =
        crate::time::deadline_timer_id(run_id).map_err(|error| EngineError::Runtime {
            reason: format!("failed to mint deadline timer id: {error}"),
        })?;
    recorder
        .record_timer_started(started_at, deadline_id.clone(), fire_at)
        .await?;
    Ok(Some((deadline_id, fire_at)))
}

/// The deadline fire time for a run started at `started_at` with `timeout`.
///
/// Deterministic: derived from the same `started_at` recorded on
/// `WorkflowStarted`, never a second clock read.
///
/// # Errors
///
/// Returns [`EngineError::Runtime`] when `timeout` is out of `chrono` range or
/// the addition overflows the representable timestamp range.
fn deadline_fire_at(
    started_at: chrono::DateTime<Utc>,
    timeout: std::time::Duration,
) -> Result<chrono::DateTime<Utc>, EngineError> {
    let delta = chrono::Duration::from_std(timeout).map_err(|error| EngineError::Runtime {
        reason: format!("declared workflow timeout is out of range: {error}"),
    })?;
    started_at
        .checked_add_signed(delta)
        .ok_or_else(|| EngineError::Runtime {
            reason: String::from("declared workflow timeout overflowed the deadline fire time"),
        })
}

/// Persist the durable deadline row and arm the live wheel for a resident run.
///
/// The durable timer row and the live-wheel task are both established here,
/// before the start is acknowledged. Failure is returned to the caller — the
/// start path fails the start rather than proceeding with an inert deadline; the
/// recorded `TimerStarted` remains the recovery anchor for a re-drive.
///
/// # Errors
///
/// Returns [`EngineError`] when the timer bridge is unavailable or the durable
/// row/wheel could not be armed.
async fn arm_declared_deadline(
    context: &StartWorkflowContext,
    workflow_id: &WorkflowId,
    deadline_id: aion_core::TimerId,
    fire_at: chrono::DateTime<Utc>,
) -> Result<(), EngineError> {
    let timer_service =
        crate::runtime::nif_timer_bridge::installed_timer_service(context.runtime.nif_state())
            .map_err(|error| EngineError::Runtime {
                reason: format!(
                    "timer service unavailable while arming workflow deadline: {error}"
                ),
            })?;
    timer_service
        .schedule(workflow_id.clone(), deadline_id.clone(), fire_at)
        .await
        .map_err(|error| EngineError::Runtime {
            reason: format!("failed to arm workflow deadline {deadline_id}: {error}"),
        })
}

/// Publishes the started handle into the active registry (with the test-only
/// start-publication pause), retracting a partial publication if the insert
/// fails.
fn publish_started_handle(
    context: &StartWorkflowContext,
    handle: &WorkflowHandle,
) -> Result<(), EngineError> {
    let pid = handle.pid();
    if let Err(error) = context
        .registry
        .insert(
            (handle.workflow_id().clone(), handle.run_id().clone()),
            handle.clone(),
        )
        .map(|_| ())
    {
        retract_registry_publication(context, handle);
        return Err(abort_unmonitored_start(&context.runtime, pid, error));
    }
    #[cfg(test)]
    context.runtime.pause_at_start_publication_for_test(pid)?;
    Ok(())
}

/// Installs the completion monitor for an already-published handle, retracting
/// the registry publication if installation fails.
fn install_started_monitor(
    context: &StartWorkflowContext,
    handle: &WorkflowHandle,
) -> Result<(), EngineError> {
    if let Err(error) = install_completion_monitor(context, handle.pid(), handle) {
        // The handle is already published; do not retract ownership until the
        // process is confirmed terminated. `fail_started_run` aborts first and
        // retracts only on success, retaining ownership otherwise.
        return Err(fail_started_run(context, handle, error));
    }
    Ok(())
}

/// Best-effort retraction of a handle's registry publication during a failed
/// start; a failure to remove is logged (the entry is superseded on re-drive).
fn retract_registry_publication(context: &StartWorkflowContext, handle: &WorkflowHandle) {
    if let Err(error) = context
        .registry
        .remove(handle.workflow_id(), handle.run_id())
    {
        tracing::warn!(
            workflow_pid = handle.pid(),
            error = %error,
            "failed to retract workflow registry publication during failed start"
        );
    }
}

fn abort_unmonitored_start(
    runtime: &Arc<RuntimeHandle>,
    pid: crate::Pid,
    cause: EngineError,
) -> EngineError {
    match runtime.abort_unmonitored_process(pid) {
        Ok(()) => cause,
        Err(abort_error) => {
            tracing::error!(pid, error = %abort_error, cause = %cause, "bounded workflow abort failed after start registration error");
            abort_error.into_engine_error()
        }
    }
}

fn install_completion_monitor(
    context: &StartWorkflowContext,
    pid: crate::Pid,
    handle: &WorkflowHandle,
) -> Result<(), EngineError> {
    let completion_context = ProcessExitContext {
        store: Arc::clone(&context.store),
        visibility_store: Arc::clone(&context.visibility_store),
        registry: Arc::clone(&context.registry),
        catalog: Arc::clone(&context.catalog),
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

fn deliver_deferred_signals(context: &StartWorkflowContext, handle: &WorkflowHandle) {
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
    ) -> Result<crate::engine_seam::RecordOutcome, EngineSeamError> {
        let _ = (workflow_id, event);
        Err(EngineSeamError::Recorder {
            reason: "start resume handoff cannot record workflow events".to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod start_tests;
