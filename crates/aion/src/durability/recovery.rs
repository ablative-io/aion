//! Recovery: enumerate active workflows and replay-and-resume on startup.

use std::sync::Arc;

use aion_core::{Event, RunId, WorkflowId};
use aion_package::ContentHash;
use aion_store::EventStore;
use chrono::{DateTime, Utc};

use crate::durability::{
    Command, DurabilityError, LiveExecutor, Recorder, Replay, ReplayOutcome, ReplayTerminal,
    Resolution, fail_on_violation,
};
use crate::{EngineError, LoadedWorkflows, Pid};

/// AE-provided replay inputs for one active workflow.
#[derive(Clone, Debug)]
pub struct RecoveryPlan {
    /// Concrete run id recovered from process/package metadata.
    pub run_id: RunId,
    /// Deterministic command stream emitted by the workflow process during replay.
    pub commands: Vec<Command>,
    /// Timestamp to use if recovery records a deterministic non-determinism failure.
    pub failure_recorded_at: DateTime<Utc>,
}

/// Small AE/test seam that supplies the execution-specific inputs AD cannot infer from history.
pub trait RecoveryDriver: Send + Sync {
    /// Builds the replay plan for one active workflow history.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] when the run id or replay command stream cannot be recovered.
    fn recovery_plan(
        &self,
        workflow_id: &WorkflowId,
        history: &[Event],
    ) -> Result<RecoveryPlan, DurabilityError>;
}

/// Resume-live handoff point reached by startup recovery.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryResumePoint {
    /// Zero-based command index of the live handoff.
    pub command_index: usize,
    /// Command AE must execute live after recovery.
    pub command: Command,
    /// Sequence head derived from recorded history before live appends resume.
    pub head: u64,
}

/// Per-workflow recovery outcome. Errors are intentionally not equatable because
/// [`DurabilityError`] preserves source error types.
#[derive(Debug)]
pub enum RecoveryOutcome {
    /// Replay reached the live handoff point without executing the live side effect.
    Resumed {
        /// Resume-live command and recovered history head.
        resume_point: RecoveryResumePoint,
        /// Recorded resolutions returned before the resume point.
        recorded: Vec<Resolution>,
    },
    /// Replay reached a recorded terminal state.
    Terminal {
        /// Terminal state from history.
        terminal: ReplayTerminal,
        /// Recorded resolutions returned before the terminal state.
        recorded: Vec<Resolution>,
        /// Sequence head derived from recorded history.
        head: u64,
    },
    /// This workflow failed recovery; other active workflows are still attempted.
    Failed {
        /// Failure raised while reading, planning, or replaying this workflow.
        error: DurabilityError,
        /// Whether recovery appended the deterministic `WorkflowFailed` event for a violation.
        failure_recorded: bool,
    },
}

/// One workflow's startup recovery report entry.
#[derive(Debug)]
pub struct RecoveryReport {
    /// Workflow recovered or attempted.
    pub workflow_id: WorkflowId,
    /// Isolated outcome for this workflow.
    pub outcome: RecoveryOutcome,
}

/// Recovers every workflow reported active by the store.
///
/// # Errors
///
/// Returns an error only when the store cannot enumerate active workflows. Per-workflow read,
/// planning, replay, and deterministic-failure-recording errors are captured in the returned report.
pub async fn recover(
    store: Arc<dyn EventStore>,
    executor: &dyn LiveExecutor,
    driver: &dyn RecoveryDriver,
) -> Result<Vec<RecoveryReport>, DurabilityError> {
    let active = store.list_active().await?;
    let mut reports = Vec::with_capacity(active.len());

    for workflow_id in active {
        let outcome = recover_one(Arc::clone(&store), executor, driver, &workflow_id)
            .await
            .unwrap_or_else(|error| RecoveryOutcome::Failed {
                error,
                failure_recorded: false,
            });
        reports.push(RecoveryReport {
            workflow_id,
            outcome,
        });
    }

    Ok(reports)
}

async fn recover_one(
    store: Arc<dyn EventStore>,
    executor: &dyn LiveExecutor,
    driver: &dyn RecoveryDriver,
    workflow_id: &WorkflowId,
) -> Result<RecoveryOutcome, DurabilityError> {
    let history = store.read_history(workflow_id).await?;
    let head = history.last().map(Event::seq).unwrap_or_default();
    let mut recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), head);
    let plan = driver.recovery_plan(workflow_id, &history)?;
    let mut replay = Replay::with_handoff(workflow_id, &plan.run_id, history, &recorder, executor)?;

    match replay.drive(plan.commands) {
        Ok(ReplayOutcome::ResumeLive {
            command_index,
            command,
            recorded,
        }) => Ok(RecoveryOutcome::Resumed {
            resume_point: RecoveryResumePoint {
                command_index,
                command,
                head,
            },
            recorded,
        }),
        Ok(ReplayOutcome::Terminal { terminal, recorded }) => Ok(RecoveryOutcome::Terminal {
            terminal,
            recorded,
            head,
        }),
        Ok(ReplayOutcome::AwaitingCommand { recorded }) => Err(DurabilityError::HistoryShape {
            reason: format!(
                "recovery command stream ended before workflow {workflow_id} reached terminal or resume point after {} recorded resolutions at head {head}",
                recorded.len()
            ),
        }),
        Err(DurabilityError::NonDeterminism(violation)) => {
            fail_on_violation(&mut recorder, plan.failure_recorded_at, &violation).await?;
            Ok(RecoveryOutcome::Failed {
                error: DurabilityError::NonDeterminism(violation),
                failure_recorded: true,
            })
        }
        Err(error) => Err(error),
    }
}

/// Process metadata reconstructed by AD while the engine builder enumerates
/// active workflow histories.
///
/// AE-011 deliberately does not implement replay. The builder reads active
/// histories, extracts the durable workflow type, then delegates to this seam to
/// recover the concrete run identifier, package version, and runtime process id
/// that should be registered as live.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveWorkflowRecovery {
    /// Concrete run being recovered for the logical workflow id.
    pub run_id: RunId,
    /// Package content hash/version that this run started on.
    pub loaded_version: ContentHash,
    /// Runtime process id recovered or spawned by AD replay.
    pub pid: Pid,
}

/// AD recovery/replay hook invoked by [`crate::EngineBuilder::build`].
pub trait ActiveWorkflowRecoverySeam: Send + Sync {
    /// Recover one active workflow's runtime metadata from durable history.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when replay metadata is unavailable or AD cannot
    /// recover the workflow process.
    fn recover_active_workflow(
        &self,
        workflow_id: &WorkflowId,
        workflow_type: &str,
        history: &[Event],
        loaded_workflows: &LoadedWorkflows,
    ) -> Result<ActiveWorkflowRecovery, EngineError>;
}

/// Placeholder AD seam for this cluster.
///
/// Later AD work replaces this object with replay that derives the run id,
/// started package version, and recovered workflow process. Returning a typed
/// load error is intentional: AE-011 must not invent a run id or pick the latest
/// loaded package version for active durable workflows.
#[derive(Debug, Default)]
pub struct DeferredActiveWorkflowRecovery;

impl ActiveWorkflowRecoverySeam for DeferredActiveWorkflowRecovery {
    fn recover_active_workflow(
        &self,
        workflow_id: &WorkflowId,
        workflow_type: &str,
        history: &[Event],
        loaded_workflows: &LoadedWorkflows,
    ) -> Result<ActiveWorkflowRecovery, EngineError> {
        let _ = (history, loaded_workflows);
        Err(EngineError::Load {
            reason: format!(
                "active workflow `{workflow_id}` of type `{workflow_type}` requires AD replay metadata before builder recovery can register it"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use aion_core::{
        Event, EventEnvelope, Payload, RunId, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary,
    };
    use aion_store::{EventStore, RunSummary, StoreError, TimerEntry};
    use async_trait::async_trait;
    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{RecoveryDriver, RecoveryPlan, recover};
    use crate::durability::{
        Command, CorrelationKey, DurabilityError, LiveActivityOutcome, LiveChildOutcome,
        LiveExecutor, RecoveryOutcome,
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[derive(Default)]
    struct CountingStore {
        active: Mutex<Vec<WorkflowId>>,
        histories: Mutex<HashMap<WorkflowId, Vec<Event>>>,
        reads: Mutex<Vec<WorkflowId>>,
    }

    #[async_trait]
    impl EventStore for CountingStore {
        async fn append(
            &self,
            workflow_id: &WorkflowId,
            events: &[Event],
            expected_seq: u64,
        ) -> Result<(), StoreError> {
            let mut histories = self
                .histories
                .lock()
                .map_err(|error| StoreError::Backend(format!("history lock poisoned: {error}")))?;
            let current = histories
                .get(workflow_id)
                .and_then(|history| history.last())
                .map(Event::seq)
                .unwrap_or_default();
            if current != expected_seq {
                return Err(StoreError::SequenceConflict {
                    expected: expected_seq,
                    found: current,
                });
            }
            histories
                .entry(workflow_id.clone())
                .or_default()
                .extend(events.iter().cloned());
            Ok(())
        }

        async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
            self.reads
                .lock()
                .map_err(|error| StoreError::Backend(format!("read lock poisoned: {error}")))?
                .push(workflow_id.clone());
            Ok(self
                .histories
                .lock()
                .map_err(|error| StoreError::Backend(format!("history lock poisoned: {error}")))?
                .get(workflow_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn read_run_chain(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<RunSummary>, StoreError> {
            let history = self.read_history(workflow_id).await?;
            aion_store::run_chain::run_chain_from_history(&history)
        }

        async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
            Ok(self
                .active
                .lock()
                .map_err(|error| StoreError::Backend(format!("active lock poisoned: {error}")))?
                .clone())
        }

        async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
            let _ = filter;
            Ok(Vec::new())
        }

        async fn schedule_timer(
            &self,
            workflow_id: &WorkflowId,
            timer_id: &TimerId,
            fire_at: DateTime<Utc>,
        ) -> Result<(), StoreError> {
            let _ = (workflow_id, timer_id, fire_at);
            Ok(())
        }

        async fn expired_timers(
            &self,
            as_of: DateTime<Utc>,
        ) -> Result<Vec<TimerEntry>, StoreError> {
            let _ = as_of;
            Ok(Vec::new())
        }
    }

    struct StaticDriver;

    impl RecoveryDriver for StaticDriver {
        fn recovery_plan(
            &self,
            workflow_id: &WorkflowId,
            history: &[Event],
        ) -> Result<RecoveryPlan, DurabilityError> {
            let _ = history;
            let activity_type = format!("activity-{workflow_id}");
            Ok(RecoveryPlan {
                run_id: RunId::new(Uuid::from_u128(10)),
                commands: vec![Command::RunActivity {
                    key: CorrelationKey::Activity(0),
                    activity_type,
                    input: payload("activity-input")?,
                }],
                failure_recorded_at: timestamp(99)?,
            })
        }
    }

    struct NoLiveExecutor;

    #[async_trait]
    impl LiveExecutor for NoLiveExecutor {
        async fn run_activity(
            &self,
            activity_type: String,
            input: Payload,
        ) -> Result<LiveActivityOutcome, DurabilityError> {
            let _ = (activity_type, input);
            Err(DurabilityError::HistoryShape {
                reason: "recovery replay must not execute live activity".to_owned(),
            })
        }

        async fn start_timer(
            &self,
            timer_id: TimerId,
            fire_at: DateTime<Utc>,
        ) -> Result<(), DurabilityError> {
            let _ = (timer_id, fire_at);
            Err(DurabilityError::HistoryShape {
                reason: "recovery replay must not execute live timer".to_owned(),
            })
        }

        async fn await_signal(
            &self,
            name: String,
            index: usize,
        ) -> Result<Payload, DurabilityError> {
            let _ = (name, index);
            Err(DurabilityError::HistoryShape {
                reason: "recovery replay must not execute live signal".to_owned(),
            })
        }

        async fn spawn_child(
            &self,
            workflow_type: String,
            input: Payload,
        ) -> Result<LiveChildOutcome, DurabilityError> {
            let _ = (workflow_type, input);
            Err(DurabilityError::HistoryShape {
                reason: "recovery replay must not execute live child".to_owned(),
            })
        }
    }

    fn timestamp(seconds: i64) -> Result<DateTime<Utc>, DurabilityError> {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| DurabilityError::HistoryShape {
                reason: format!("invalid timestamp {seconds}"),
            })
    }

    fn payload(label: &str) -> Result<Payload, DurabilityError> {
        Payload::from_json(&json!({ "label": label })).map_err(|error| {
            DurabilityError::HistoryShape {
                reason: format!("invalid test payload: {error}"),
            }
        })
    }

    fn started_event(workflow_id: WorkflowId) -> Result<Event, DurabilityError> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: timestamp(1)?,
                workflow_id,
            },
            workflow_type: "workflow".to_owned(),
            input: payload("input")?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
        })
    }

    #[tokio::test]
    async fn recover_lists_active_and_reads_each_history() -> TestResult {
        let first = WorkflowId::new(Uuid::from_u128(1));
        let second = WorkflowId::new(Uuid::from_u128(2));
        let store = Arc::new(CountingStore::default());
        store
            .active
            .lock()
            .map_err(|error| format!("active lock poisoned: {error}"))?
            .extend([first.clone(), second.clone()]);
        store
            .histories
            .lock()
            .map_err(|error| format!("history lock poisoned: {error}"))?
            .extend([
                (first.clone(), vec![started_event(first.clone())?]),
                (second.clone(), vec![started_event(second.clone())?]),
            ]);
        let store_for_recovery: Arc<dyn EventStore> = store.clone();

        let report = recover(store_for_recovery, &NoLiveExecutor, &StaticDriver).await?;

        assert_eq!(report.len(), 2);
        assert!(
            report
                .iter()
                .all(|entry| matches!(entry.outcome, RecoveryOutcome::Resumed { .. }))
        );
        let reads = store
            .reads
            .lock()
            .map_err(|error| format!("read lock poisoned: {error}"))?
            .clone();
        assert_eq!(reads, vec![first, second]);
        Ok(())
    }
}
