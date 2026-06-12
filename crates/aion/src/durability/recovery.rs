//! Recovery: enumerate active workflows and replay-and-resume on startup.

use std::sync::Arc;

use aion_core::{Event, PackageVersion, RunId, WorkflowId};
use aion_package::ContentHash;
use aion_store::EventStore;
use chrono::{DateTime, Utc};

use crate::durability::{
    Command, DurabilityError, LiveExecutor, Recorder, Replay, ReplayOutcome, ReplayTerminal,
    Resolution, fail_on_violation,
};
use crate::supervision::spawn_workflow_with_policy;
use crate::{EngineError, Pid, RuntimeHandle, RuntimeInput, WorkflowCatalog};

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

struct StartedMetadata<'a> {
    input: &'a aion_core::Payload,
    run_id: &'a RunId,
    package_version: &'a PackageVersion,
}

fn started_metadata<'a>(
    workflow_id: &WorkflowId,
    expected_workflow_type: &str,
    history: &'a [Event],
) -> Result<StartedMetadata<'a>, EngineError> {
    let Some((workflow_type, input, run_id, package_version)) =
        history.iter().rev().find_map(|event| match event {
            Event::WorkflowStarted {
                workflow_type,
                input,
                run_id,
                package_version,
                ..
            } => Some((workflow_type, input, run_id, package_version)),
            _ => None,
        })
    else {
        return Err(EngineError::Load {
            reason: format!(
                "active workflow `{workflow_id}` has no WorkflowStarted event in durable history"
            ),
        });
    };

    if workflow_type != expected_workflow_type {
        return Err(EngineError::Load {
            reason: format!(
                "active workflow `{workflow_id}` started as `{workflow_type}` but recovery was requested for `{expected_workflow_type}`"
            ),
        });
    }

    Ok(StartedMetadata {
        input,
        run_id,
        package_version,
    })
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
pub enum ActiveWorkflowRecovery {
    /// A normal workflow recovered as a resident runtime process.
    Resident {
        /// Concrete run being recovered for the logical workflow id.
        run_id: RunId,
        /// Package content hash/version that this run started on.
        loaded_version: ContentHash,
        /// Runtime process id recovered or spawned by AD replay.
        pid: Pid,
    },
    /// The virtual schedule coordinator history recovered without a runtime process.
    ScheduleCoordinator {
        /// Concrete run recorded by the coordinator's lifecycle event.
        run_id: RunId,
    },
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
        catalog: &WorkflowCatalog,
    ) -> Result<ActiveWorkflowRecovery, EngineError>;
}

/// Production AD seam that resumes an active workflow by spawning its loaded
/// workflow entrypoint against the existing durable history.
///
/// Per-NIF context construction resolves commands from the recorded event
/// history before any live side effect runs, so the restored process advances
/// through replay using the same resolver path as normal execution. This seam
/// only reconstructs immutable start metadata and a runtime PID; the builder
/// remains responsible for constructing the single resumed [`Recorder`].
pub struct ActiveWorkflowRecoverySeamImpl {
    runtime: Arc<RuntimeHandle>,
}

impl ActiveWorkflowRecoverySeamImpl {
    /// Build a production recovery seam bound to the runtime that has loaded the
    /// workflow packages for this engine instance.
    #[must_use]
    pub fn new(runtime: Arc<RuntimeHandle>) -> Self {
        Self { runtime }
    }
}

impl ActiveWorkflowRecoverySeam for ActiveWorkflowRecoverySeamImpl {
    fn recover_active_workflow(
        &self,
        workflow_id: &WorkflowId,
        workflow_type: &str,
        history: &[Event],
        catalog: &WorkflowCatalog,
    ) -> Result<ActiveWorkflowRecovery, EngineError> {
        let started = started_metadata(workflow_id, workflow_type, history)?;
        let version = crate::loader::parse_package_version(workflow_type, started.package_version)?;
        let loaded = catalog
            .get(workflow_type, &version)?
            .ok_or_else(|| EngineError::Load {
                reason: format!(
                    "active workflow `{workflow_id}` is pinned to package version `{version}` of `{workflow_type}`, which is not loaded"
                ),
            })?;
        let runtime_input = RuntimeInput::from_payload(started.input)?;
        let pid = spawn_workflow_with_policy(
            &self.runtime,
            loaded.deployed_entry_module(),
            loaded.entry_function(),
            runtime_input,
        )?;

        Ok(ActiveWorkflowRecovery::Resident {
            run_id: started.run_id.clone(),
            loaded_version: loaded.version().clone(),
            pid,
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
    use aion_store::{
        EventStore, ReadableEventStore, RunSummary, StoreError, TimerEntry, WritableEventStore,
        WriteToken,
    };
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
    impl WritableEventStore for CountingStore {
        async fn append(
            &self,
            _token: WriteToken,
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
    }

    #[async_trait]
    impl ReadableEventStore for CountingStore {
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

        async fn read_history_from(
            &self,
            workflow_id: &WorkflowId,
            from_seq: u64,
        ) -> Result<Vec<Event>, StoreError> {
            let history = self.read_history(workflow_id).await?;
            Ok(history
                .into_iter()
                .filter(|event| event.seq() >= from_seq)
                .collect())
        }

        async fn read_run_chain(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<RunSummary>, StoreError> {
            let history = self.read_history(workflow_id).await?;
            aion_store::run_chain::run_chain_from_history(&history)
        }

        async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
            let mut workflow_ids = self
                .histories
                .lock()
                .map_err(|error| StoreError::Backend(format!("history lock poisoned: {error}")))?
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            workflow_ids.sort_by_key(ToString::to_string);
            Ok(workflow_ids)
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

    /// Recovery never touches deployed packages: reads are legitimately
    /// empty, mutations are unexpected.
    #[async_trait]
    impl aion_store::PackageStore for CountingStore {
        async fn put_package(&self, record: aion_store::PackageRecord) -> Result<(), StoreError> {
            let _ = record;
            Err(StoreError::Backend(
                "unexpected put_package in the recovery test".to_owned(),
            ))
        }

        async fn list_packages(&self) -> Result<Vec<aion_store::PackageRecord>, StoreError> {
            Ok(Vec::new())
        }

        async fn delete_package(
            &self,
            workflow_type: &str,
            content_hash: &str,
        ) -> Result<(), StoreError> {
            let _ = (workflow_type, content_hash);
            Err(StoreError::Backend(
                "unexpected delete_package in the recovery test".to_owned(),
            ))
        }

        async fn put_package_route(
            &self,
            workflow_type: &str,
            content_hash: &str,
        ) -> Result<(), StoreError> {
            let _ = (workflow_type, content_hash);
            Err(StoreError::Backend(
                "unexpected put_package_route in the recovery test".to_owned(),
            ))
        }

        async fn list_package_routes(
            &self,
        ) -> Result<Vec<aion_store::PackageRouteRecord>, StoreError> {
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
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(10)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
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
