//! `Engine` durable-schedule surface: create/update/pause/resume/delete,
//! listing and description, timer-fired handling, startup recovery, and the
//! schedule coordinator's evaluator/recorder assembly.

use std::collections::HashMap;
use std::sync::Arc;

use aion_core::{
    Event, EventEnvelope, Payload, RunId, ScheduleConfig, ScheduleId, SearchAttributeSchema,
    SearchAttributeValue, WorkflowId,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex as AsyncMutex;

use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;

use crate::durability::Recorder;
use crate::lifecycle::start::{self, StartWorkflowContext};
use crate::schedule::{
    NoopScheduleCanceller, ScheduleEvaluator, ScheduleEvaluatorError, ScheduleEventSink,
    ScheduleEventSource, ScheduleExecution, ScheduleState, ScheduleTimer, ScheduleWorkflowStarter,
    StoreScheduleTimer, TimerEvaluationOutcome,
};
use crate::{EngineError, LoadedWorkflows, Registry, RuntimeHandle, SupervisionTree};

use super::api::Engine;

impl Engine {
    /// Workflow history used for durable schedule events and timer ownership.
    #[must_use]
    pub const fn schedule_coordinator_workflow_id(&self) -> &WorkflowId {
        &self.schedule_coordinator_workflow_id
    }

    /// Create a durable schedule and arm its first timer.
    ///
    /// # Errors
    ///
    /// Returns shutdown, durability, schedule projection, or timer arming errors.
    pub async fn create_schedule(&self, config: ScheduleConfig) -> Result<ScheduleId, EngineError> {
        let operation = self.shutdown_gate.begin_start()?;
        let recorded_at = Utc::now();
        let schedule_id = ScheduleId::new_v4();
        let result = self
            .create_schedule_inner(schedule_id, config, recorded_at)
            .await;
        drop(operation);
        result
    }

    /// Update an existing schedule's configuration and re-arm it.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins,
    /// [`EngineError::ScheduleNotFound`] for absent/deleted schedules, or typed durability,
    /// projection, and timer errors.
    pub async fn update_schedule(
        &self,
        schedule_id: &ScheduleId,
        config: ScheduleConfig,
    ) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = self.update_schedule_inner(schedule_id, config).await;
        drop(operation);
        result
    }

    /// Pause an existing schedule.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins,
    /// [`EngineError::ScheduleNotFound`] for absent/deleted schedules, or typed durability
    /// and projection errors.
    pub async fn pause_schedule(&self, schedule_id: &ScheduleId) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = self.pause_schedule_inner(schedule_id).await;
        drop(operation);
        result
    }

    /// Resume an existing schedule and re-arm it.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins,
    /// [`EngineError::ScheduleNotFound`] for absent/deleted schedules, or typed durability,
    /// projection, and timer errors.
    pub async fn resume_schedule(&self, schedule_id: &ScheduleId) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = self.resume_schedule_inner(schedule_id).await;
        drop(operation);
        result
    }

    /// Delete an existing schedule so it is no longer listed or armed.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins,
    /// [`EngineError::ScheduleNotFound`] for absent/deleted schedules, or typed durability
    /// and projection errors.
    pub async fn delete_schedule(&self, schedule_id: &ScheduleId) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = self.delete_schedule_inner(schedule_id).await;
        drop(operation);
        result
    }

    /// List all non-deleted schedules from projected state.
    ///
    /// # Errors
    ///
    /// Currently returns only infallible projected state, wrapped for API consistency.
    pub async fn list_schedules(&self) -> Result<Vec<ScheduleState>, EngineError> {
        let evaluator = self.schedule_evaluator.lock().await;
        let mut schedules = evaluator
            .states()
            .filter(|state| !state.is_deleted)
            .cloned()
            .collect::<Vec<_>>();
        schedules.sort_by(|left, right| {
            left.next_trigger_at
                .cmp(&right.next_trigger_at)
                .then_with(|| {
                    left.schedule_id
                        .to_string()
                        .cmp(&right.schedule_id.to_string())
                })
        });
        Ok(schedules)
    }

    /// Describe one non-deleted schedule.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ScheduleNotFound`] for absent or deleted schedules.
    pub async fn describe_schedule(
        &self,
        schedule_id: &ScheduleId,
    ) -> Result<ScheduleState, EngineError> {
        self.schedule_evaluator
            .lock()
            .await
            .state(schedule_id)
            .filter(|state| !state.is_deleted)
            .cloned()
            .ok_or_else(|| EngineError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            })
    }

    async fn create_schedule_inner(
        &self,
        schedule_id: ScheduleId,
        config: ScheduleConfig,
        recorded_at: DateTime<Utc>,
    ) -> Result<ScheduleId, EngineError> {
        self.schedule_recorder
            .lock()
            .await
            .record_schedule_created(recorded_at, schedule_id.clone(), config.clone())
            .await?;

        let state = ScheduleState::created(schedule_id.clone(), config, recorded_at)?;
        let mut evaluator = self.schedule_evaluator.lock().await;
        evaluator.upsert_state(state);
        evaluator.arm_active_schedule(&schedule_id).await?;
        Ok(schedule_id)
    }

    async fn update_schedule_inner(
        &self,
        schedule_id: &ScheduleId,
        config: ScheduleConfig,
    ) -> Result<(), EngineError> {
        self.ensure_schedule_exists(schedule_id).await?;
        let recorded_at = Utc::now();
        self.schedule_recorder
            .lock()
            .await
            .record_schedule_updated(recorded_at, schedule_id.clone(), config.clone())
            .await?;
        let event = Event::ScheduleUpdated {
            envelope: schedule_event_envelope(recorded_at),
            schedule_id: schedule_id.clone(),
            config,
        };
        self.apply_schedule_event(schedule_id, &event, true).await
    }

    async fn pause_schedule_inner(&self, schedule_id: &ScheduleId) -> Result<(), EngineError> {
        self.ensure_schedule_exists(schedule_id).await?;
        let recorded_at = Utc::now();
        self.schedule_recorder
            .lock()
            .await
            .record_schedule_paused(recorded_at, schedule_id.clone())
            .await?;
        let event = Event::SchedulePaused {
            envelope: schedule_event_envelope(recorded_at),
            schedule_id: schedule_id.clone(),
        };
        self.apply_schedule_event(schedule_id, &event, false).await
    }

    async fn resume_schedule_inner(&self, schedule_id: &ScheduleId) -> Result<(), EngineError> {
        self.ensure_schedule_exists(schedule_id).await?;
        let recorded_at = Utc::now();
        self.schedule_recorder
            .lock()
            .await
            .record_schedule_resumed(recorded_at, schedule_id.clone())
            .await?;
        let event = Event::ScheduleResumed {
            envelope: schedule_event_envelope(recorded_at),
            schedule_id: schedule_id.clone(),
        };
        self.apply_schedule_event(schedule_id, &event, true).await
    }

    async fn delete_schedule_inner(&self, schedule_id: &ScheduleId) -> Result<(), EngineError> {
        self.ensure_schedule_exists(schedule_id).await?;
        let recorded_at = Utc::now();
        self.schedule_recorder
            .lock()
            .await
            .record_schedule_deleted(recorded_at, schedule_id.clone())
            .await?;
        let event = Event::ScheduleDeleted {
            envelope: schedule_event_envelope(recorded_at),
            schedule_id: schedule_id.clone(),
        };
        self.apply_schedule_event(schedule_id, &event, false).await
    }

    async fn ensure_schedule_exists(&self, schedule_id: &ScheduleId) -> Result<(), EngineError> {
        self.schedule_evaluator
            .lock()
            .await
            .state(schedule_id)
            .filter(|state| !state.is_deleted)
            .map(|_| ())
            .ok_or_else(|| EngineError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            })
    }

    async fn apply_schedule_event(
        &self,
        schedule_id: &ScheduleId,
        event: &Event,
        should_arm: bool,
    ) -> Result<(), EngineError> {
        let mut evaluator = self.schedule_evaluator.lock().await;
        let mut state = evaluator
            .state(schedule_id)
            .filter(|state| !state.is_deleted)
            .cloned()
            .ok_or_else(|| EngineError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            })?;
        state.apply(event)?;
        evaluator.upsert_state(state);
        if should_arm {
            evaluator.arm_active_schedule(schedule_id).await?;
        }
        Ok(())
    }

    /// Handles a fired durable schedule timer through the schedule evaluator.
    ///
    /// # Errors
    ///
    /// Returns schedule evaluator or shutdown errors.
    pub async fn handle_schedule_timer_fired(
        &self,
        schedule_id: &ScheduleId,
        fire_at: DateTime<Utc>,
    ) -> Result<TimerEvaluationOutcome, EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = self
            .schedule_evaluator
            .lock()
            .await
            .handle_timer_fired(schedule_id, fire_at)
            .await
            .map_err(EngineError::from);
        drop(operation);
        result
    }

    /// Rebuilds schedule state from durable coordinator history and re-arms active schedules.
    ///
    /// # Errors
    ///
    /// Returns schedule projection, catch-up, timer, or workflow-start errors.
    pub async fn recover_schedules_on_startup(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(), EngineError> {
        let source = StoreScheduleEventSource {
            store: self.store(),
            coordinator_workflow_id: self.schedule_coordinator_workflow_id.clone(),
        };
        self.schedule_evaluator
            .lock()
            .await
            .recover_on_startup(&source, now)
            .await
            .map_err(EngineError::from)
    }
}

pub(crate) fn schedule_coordinator_workflow_id() -> WorkflowId {
    WorkflowId::new(uuid::Uuid::from_u128(
        0x0000_0000_a10a_0000_0000_0000_0000_0004,
    ))
}

pub(crate) fn schedule_coordinator_run_id() -> RunId {
    RunId::new(uuid::Uuid::from_u128(
        0x0000_0000_a10a_0000_0000_0000_0000_0005,
    ))
}

pub(crate) const fn schedule_coordinator_workflow_type() -> &'static str {
    "aion.schedule_coordinator"
}

fn schedule_event_envelope(recorded_at: DateTime<Utc>) -> EventEnvelope {
    EventEnvelope {
        seq: 0,
        recorded_at,
        workflow_id: schedule_coordinator_workflow_id(),
    }
}

pub(super) fn default_schedule_evaluator(
    coordinator_workflow_id: WorkflowId,
    recorder: Arc<AsyncMutex<Recorder>>,
    deps: ScheduleRuntimeDeps,
) -> ScheduleEvaluator {
    let timer: Arc<dyn ScheduleTimer> = Arc::new(StoreScheduleTimer::new(
        Arc::clone(&deps.store),
        coordinator_workflow_id,
    ));
    let starter: Arc<dyn ScheduleWorkflowStarter> = Arc::new(EngineScheduleStarter { deps });
    let canceller: Arc<dyn crate::schedule::ScheduleWorkflowCanceller> =
        Arc::new(NoopScheduleCanceller);
    let events: Arc<dyn ScheduleEventSink> = recorder;
    ScheduleEvaluator::new(timer, starter, canceller, events)
}

pub(super) struct ScheduleRuntimeDeps {
    pub(super) store: Arc<dyn EventStore>,
    pub(super) visibility_store: Arc<dyn VisibilityStore>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) loaded_workflows: LoadedWorkflows,
    pub(super) registry: Arc<Registry>,
    pub(super) supervision: Arc<SupervisionTree>,
    pub(super) search_attribute_schema: Arc<SearchAttributeSchema>,
}

struct EngineScheduleStarter {
    deps: ScheduleRuntimeDeps,
}

#[async_trait]
impl ScheduleWorkflowStarter for EngineScheduleStarter {
    async fn start_scheduled_workflow(
        &self,
        workflow_type: &str,
        input: Payload,
        search_attributes: HashMap<String, SearchAttributeValue>,
    ) -> Result<ScheduleExecution, ScheduleEvaluatorError> {
        let handle = start::start_workflow_with_options(
            StartWorkflowContext {
                store: Arc::clone(&self.deps.store),
                visibility_store: Arc::clone(&self.deps.visibility_store),
                loaded_workflows: &self.deps.loaded_workflows,
                runtime: Arc::clone(&self.deps.runtime),
                supervision: Arc::clone(&self.deps.supervision),
                registry: Arc::clone(&self.deps.registry),
                signal_handoff: None,
                search_attribute_schema: Arc::clone(&self.deps.search_attribute_schema),
                monitor_tokio_handle: tokio::runtime::Handle::current(),
            },
            workflow_type,
            input,
            start::StartWorkflowOptions {
                search_attributes,
                ..start::StartWorkflowOptions::default()
            },
        )
        .await
        .map_err(|error| ScheduleEvaluatorError::side_effect(error.to_string()))?;
        Ok(ScheduleExecution::new(
            handle.workflow_id().clone(),
            handle.run_id().clone(),
        ))
    }
}

struct StoreScheduleEventSource {
    store: Arc<dyn EventStore>,
    coordinator_workflow_id: WorkflowId,
}

#[async_trait]
impl ScheduleEventSource for StoreScheduleEventSource {
    async fn schedule_events(&self) -> Result<Vec<Event>, ScheduleEvaluatorError> {
        self.store
            .read_history(&self.coordinator_workflow_id)
            .await
            .map_err(|error| ScheduleEvaluatorError::side_effect(error.to_string()))
    }
}

#[async_trait]
impl ScheduleEventSink for AsyncMutex<Recorder> {
    async fn record_schedule_triggered(
        &self,
        schedule_id: &ScheduleId,
        execution: &ScheduleExecution,
        recorded_at: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError> {
        self.lock()
            .await
            .record_schedule_triggered(
                recorded_at,
                schedule_id.clone(),
                execution.workflow_id.clone(),
                execution.run_id.clone(),
            )
            .await
            .map_err(|error| ScheduleEvaluatorError::side_effect(error.to_string()))
    }
}
