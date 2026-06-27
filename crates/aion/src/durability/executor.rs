//! `LiveExecutor` trait and resume-live handoff glue.

use aion_core::{ActivityError, ActivityId, Payload, TimerId, WorkflowError, WorkflowId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::durability::{
    Command, CorrelationKey, DurabilityError, Recorder, Resolution, ResolveOutcome, Resolver,
};

/// Live outcome produced by AE after running an activity for real.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveActivityOutcome {
    /// The activity completed successfully with an opaque result payload.
    Completed(Payload),
    /// The activity reached its terminal failure state after live retry policy was exhausted.
    Failed(ActivityError),
}

/// Live outcome produced by AE after spawning and awaiting a child workflow for real.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveChildOutcome {
    /// The concrete child workflow completed successfully.
    Completed {
        /// Identifier AE assigned to the child workflow instance.
        child_workflow_id: WorkflowId,
        /// Package version AE resolved for the child at spawn time.
        package_version: aion_core::PackageVersion,
        /// Opaque child result payload.
        result: Payload,
    },
    /// The concrete child workflow failed terminally.
    Failed {
        /// Identifier AE assigned to the child workflow instance.
        child_workflow_id: WorkflowId,
        /// Package version AE resolved for the child at spawn time.
        package_version: aion_core::PackageVersion,
        /// Terminal child workflow error.
        error: WorkflowError,
    },
}

impl LiveChildOutcome {
    fn package_version(&self) -> aion_core::PackageVersion {
        match self {
            Self::Completed {
                package_version, ..
            }
            | Self::Failed {
                package_version, ..
            } => package_version.clone(),
        }
    }

    fn child_workflow_id(&self) -> WorkflowId {
        match self {
            Self::Completed {
                child_workflow_id, ..
            }
            | Self::Failed {
                child_workflow_id, ..
            } => child_workflow_id.clone(),
        }
    }
}

/// Outcome returned by [`resolve_or_execute_live`] for commands at the AD/AE seam.
#[derive(Clone, Debug, PartialEq)]
pub enum HandoffOutcome {
    /// A world-touching command produced a replayed or live resolution.
    Resolved(Resolution),
    /// The workflow was durably completed; completion has no AD `Resolution` shape.
    WorkflowCompleted,
}

/// AE-provided live side-effect executor.
///
/// AD owns replay, the resolver, and event recording. AE owns actual world interaction (activity
/// dispatch, timer wheel/durable timer scheduling, signal wait plumbing, and child workflow
/// process management) and supplies an object-safe implementation of this trait, such as a
/// beamr-backed executor, without AD depending on those runtime crates.
///
/// AD calls these methods only after [`Resolver`] returns [`ResolveOutcome::ResumeLive`]. While
/// recorded history can satisfy a command, the executor must not be touched. Command-issued events
/// are recorded by [`resolve_or_execute_live`] through the single per-workflow [`Recorder`]:
/// activity scheduled/started/outcome, timer started, child workflow started, and workflow
/// completed. Asynchronous arrival events that are not workflow-issued commands (`TimerFired`,
/// `SignalReceived`, `ChildWorkflowCompleted`, and `ChildWorkflowFailed`) are recorded by AT/AE
/// services when they occur, but still through that same recorder instance so the recorder remains
/// the only sequence-head authority.
#[async_trait]
pub trait LiveExecutor: Send + Sync {
    /// Runs an activity for real at the resume-live point.
    ///
    /// # Errors
    ///
    /// Returns a durability error when the live runtime cannot produce a recordable outcome.
    async fn run_activity(
        &self,
        activity_type: String,
        input: Payload,
    ) -> Result<LiveActivityOutcome, DurabilityError>;

    /// Starts or awaits a timer for real at the resume-live point.
    ///
    /// The AE implementation arms runtime timer machinery and persists any durable timer row; AD
    /// records only the command-issued `TimerStarted` history event.
    ///
    /// # Errors
    ///
    /// Returns a durability error when the live runtime cannot start or await the timer.
    async fn start_timer(
        &self,
        timer_id: TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), DurabilityError>;

    /// Awaits a signal for real at the resume-live point.
    ///
    /// # Errors
    ///
    /// Returns a durability error when the live runtime cannot deliver a signal payload.
    async fn await_signal(&self, name: String, index: usize) -> Result<Payload, DurabilityError>;

    /// Spawns and awaits a child workflow for real at the resume-live point.
    ///
    /// # Errors
    ///
    /// Returns a durability error when the live runtime cannot produce a recordable child outcome.
    async fn spawn_child(
        &self,
        workflow_type: String,
        input: Payload,
    ) -> Result<LiveChildOutcome, DurabilityError>;
}

/// Resolves a command from history or hands it off to AE live execution and records the outcome.
///
/// This function is the resume-live handoff glue for AD-007. It first asks the resolver to satisfy
/// the command from history. If the resolver returns a recorded resolution, that resolution is
/// returned immediately and no [`LiveExecutor`] method is invoked. Only when the resolver reports
/// [`ResolveOutcome::ResumeLive`] does this function call AE, then append command-issued events
/// through the supplied single-writer [`Recorder`]. The `recorded_at` value is supplied by the
/// caller so this module does not consult wall-clock time for workflow-visible history timestamps.
///
/// # Errors
///
/// Returns resolver non-determinism/history-shape errors, live executor errors, or recorder/store
/// errors. Sequence conflicts from the recorder are surfaced directly as hard durability errors.
pub async fn resolve_or_execute_live(
    resolver: &mut Resolver,
    recorder: &mut Recorder,
    executor: &dyn LiveExecutor,
    command: Command,
    recorded_at: DateTime<Utc>,
) -> Result<HandoffOutcome, DurabilityError> {
    match resolver.resolve(command.clone())? {
        ResolveOutcome::Recorded(resolution) => Ok(HandoffOutcome::Resolved(resolution)),
        ResolveOutcome::ResumeLive => {
            execute_live_and_record(recorder, executor, command, recorded_at).await
        }
    }
}

async fn execute_live_and_record(
    recorder: &mut Recorder,
    executor: &dyn LiveExecutor,
    command: Command,
    recorded_at: DateTime<Utc>,
) -> Result<HandoffOutcome, DurabilityError> {
    match command {
        Command::RunActivity {
            key,
            activity_type,
            input,
        } => {
            let activity_id = activity_id_from_key(&key)?;
            recorder
                .record_activity_scheduled(
                    recorded_at,
                    activity_id.clone(),
                    activity_type.clone(),
                    input.clone(),
                    // No SDK-level task-queue selection yet (NSTQ-4); the single-schedule seam
                    // records the named default task queue.
                    String::from(aion_core::DEFAULT_TASK_QUEUE),
                    // No SDK-level node selection yet (NODE-4); the single-schedule seam records
                    // no node affinity (`None` = genuine current value).
                    None,
                )
                .await?;
            recorder
                .record_activity_started(recorded_at, activity_id.clone())
                .await?;
            let outcome = executor.run_activity(activity_type, input).await?;
            match outcome {
                LiveActivityOutcome::Completed(result) => {
                    recorder
                        .record_activity_completed(recorded_at, activity_id, result.clone())
                        .await?;
                    Ok(HandoffOutcome::Resolved(Resolution::ActivityCompleted(
                        result,
                    )))
                }
                LiveActivityOutcome::Failed(error) => {
                    ensure_terminal_activity_error(&error)?;
                    recorder
                        .record_activity_failed(recorded_at, activity_id, error.clone(), 1)
                        .await?;
                    Ok(HandoffOutcome::Resolved(
                        Resolution::ActivityFailedTerminal(error),
                    ))
                }
            }
        }
        Command::StartTimer { key, fire_at } => {
            let timer_id = timer_id_from_key(&key)?;
            recorder
                .record_timer_started(recorded_at, timer_id.clone(), fire_at)
                .await?;
            executor.start_timer(timer_id, fire_at).await?;
            Ok(HandoffOutcome::Resolved(Resolution::TimerFired))
        }
        Command::AwaitSignal { key } => {
            let (name, index) = signal_from_key(&key)?;
            let payload = executor.await_signal(name, index).await?;
            Ok(HandoffOutcome::Resolved(Resolution::SignalDelivered(
                payload,
            )))
        }
        Command::SendSignal { .. } => Err(DurabilityError::HistoryShape {
            reason: "send-signal live execution is owned by the NIF signal bridge".to_owned(),
        }),
        Command::AwaitChild { .. } => Err(DurabilityError::HistoryShape {
            reason: "await-child live execution is owned by the NIF child bridge".to_owned(),
        }),
        Command::SpawnChild {
            key,
            workflow_type,
            input,
        } => {
            child_from_key(&key)?;
            let outcome = executor
                .spawn_child(workflow_type.clone(), input.clone())
                .await?;
            recorder
                .record_child_workflow_started(
                    recorded_at,
                    outcome.child_workflow_id(),
                    workflow_type,
                    input,
                    outcome.package_version(),
                )
                .await?;
            match outcome {
                LiveChildOutcome::Completed { result, .. } => {
                    Ok(HandoffOutcome::Resolved(Resolution::ChildCompleted(result)))
                }
                LiveChildOutcome::Failed { error, .. } => {
                    Ok(HandoffOutcome::Resolved(Resolution::ChildFailed(error)))
                }
            }
        }
        Command::CompleteWorkflow { result } => {
            recorder
                .record_workflow_completed(recorded_at, result)
                .await?;
            Ok(HandoffOutcome::WorkflowCompleted)
        }
    }
}

fn ensure_terminal_activity_error(error: &ActivityError) -> Result<(), DurabilityError> {
    if error.is_retryable() {
        return Err(DurabilityError::HistoryShape {
            reason: "live activity failure must be terminal before AD can record a terminal \
                     resolution"
                .to_owned(),
        });
    }
    Ok(())
}

fn activity_id_from_key(key: &CorrelationKey) -> Result<ActivityId, DurabilityError> {
    match key {
        CorrelationKey::Activity(ordinal) => Ok(ActivityId::from_sequence_position(*ordinal)),
        other => Err(DurabilityError::HistoryShape {
            reason: format!("RunActivity requires an activity correlation key, got {other:?}"),
        }),
    }
}

fn timer_id_from_key(key: &CorrelationKey) -> Result<TimerId, DurabilityError> {
    match key {
        CorrelationKey::Timer(timer_id) => Ok(timer_id.clone()),
        other => Err(DurabilityError::HistoryShape {
            reason: format!("StartTimer requires a timer correlation key, got {other:?}"),
        }),
    }
}

fn signal_from_key(key: &CorrelationKey) -> Result<(String, usize), DurabilityError> {
    match key {
        CorrelationKey::Signal { name, index } => Ok((name.clone(), *index)),
        other => Err(DurabilityError::HistoryShape {
            reason: format!("AwaitSignal requires a signal correlation key, got {other:?}"),
        }),
    }
}

fn child_from_key(key: &CorrelationKey) -> Result<u64, DurabilityError> {
    match key {
        CorrelationKey::Child(ordinal) => Ok(*ordinal),
        other => Err(DurabilityError::HistoryShape {
            reason: format!("SpawnChild requires a child correlation key, got {other:?}"),
        }),
    }
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod executor_tests;
