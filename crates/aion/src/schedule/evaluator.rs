//! Timer-driven schedule evaluation service.
//!
//! The evaluator is intentionally built around explicit seams for durable timer arming, workflow
//! start/cancel side effects, and recovery event sourcing. Engine API wiring is left to later
//! schedule integration work.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aion_core::{Event, Payload, ScheduleId, SearchAttributeValue, TimerId, WorkflowId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::schedule::{
    OverlapDecision, ScheduleError, ScheduleExecution, ScheduleState, evaluate_catch_up,
    evaluate_overlap, next_fire_time, project_schedule_state,
};

#[cfg(test)]
mod tests;

/// Errors returned by schedule evaluation and recovery.
#[derive(thiserror::Error, Debug)]
pub enum ScheduleEvaluatorError {
    /// Trigger parsing or advancement failed.
    #[error("schedule trigger error: {0}")]
    Trigger(#[from] ScheduleError),

    /// A timer identifier could not be constructed for a schedule.
    #[error("schedule timer id error: {0}")]
    TimerId(#[from] aion_core::IdError),

    /// No state exists for a schedule referenced by the caller.
    #[error("schedule `{schedule_id}` was not found")]
    ScheduleNotFound {
        /// Schedule identifier requested by the caller.
        schedule_id: ScheduleId,
    },

    /// A side-effect dependency failed.
    #[error("schedule side effect failed: {reason}")]
    SideEffect {
        /// Human-readable failure reason.
        reason: String,
    },
}

impl ScheduleEvaluatorError {
    pub(crate) fn side_effect(reason: impl Into<String>) -> Self {
        Self::SideEffect {
            reason: reason.into(),
        }
    }
}

/// Durable timer seam used by [`ScheduleEvaluator`].
#[async_trait]
pub trait ScheduleTimer: Send + Sync {
    /// Arms a durable timer for the schedule's next fire time.
    async fn arm_schedule_timer(
        &self,
        schedule_id: &ScheduleId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError>;
}

/// Workflow-start seam used by [`ScheduleEvaluator`].
#[async_trait]
pub trait ScheduleWorkflowStarter: Send + Sync {
    /// Starts a workflow for a schedule tick and returns the concrete execution identifiers.
    ///
    /// `search_attributes` come from the schedule's persisted configuration and
    /// are recorded on every triggered execution so visibility metadata (such as
    /// a server-assigned tenancy attribute) survives engine restarts.
    async fn start_scheduled_workflow(
        &self,
        workflow_type: &str,
        input: Payload,
        search_attributes: HashMap<String, SearchAttributeValue>,
    ) -> Result<ScheduleExecution, ScheduleEvaluatorError>;
}

/// Workflow-cancellation seam used by [`ScheduleEvaluator`] for `CancelPrevious`.
#[async_trait]
pub trait ScheduleWorkflowCanceller: Send + Sync {
    /// Cancels a running schedule-started workflow execution.
    async fn cancel_scheduled_workflow(
        &self,
        execution: &ScheduleExecution,
        reason: &str,
    ) -> Result<(), ScheduleEvaluatorError>;
}

/// Schedule event recording seam used by [`ScheduleEvaluator`].
#[async_trait]
pub trait ScheduleEventSink: Send + Sync {
    /// Records that a schedule tick started a workflow execution.
    async fn record_schedule_triggered(
        &self,
        schedule_id: &ScheduleId,
        execution: &ScheduleExecution,
        recorded_at: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError>;
}

/// Recovery source seam for reconstructing schedule state from durable events.
#[async_trait]
pub trait ScheduleEventSource: Send + Sync {
    /// Returns schedule-bearing events in projection order.
    async fn schedule_events(&self) -> Result<Vec<Event>, ScheduleEvaluatorError>;
}

/// Result returned after handling a schedule timer tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimerEvaluationOutcome {
    /// A workflow was started and a `ScheduleTriggered` event was recorded.
    Started(ScheduleExecution),
    /// The tick was skipped by overlap policy.
    Skipped,
    /// The tick was buffered because one execution is already active.
    Buffered,
    /// The schedule is inactive and no timer was re-armed.
    Inactive,
}

/// Schedule evaluator state and dependencies.
pub struct ScheduleEvaluator {
    states: HashMap<ScheduleId, ScheduleState>,
    pending_ticks: HashSet<ScheduleId>,
    timer: Arc<dyn ScheduleTimer>,
    starter: Arc<dyn ScheduleWorkflowStarter>,
    canceller: Arc<dyn ScheduleWorkflowCanceller>,
    events: Arc<dyn ScheduleEventSink>,
}

impl ScheduleEvaluator {
    /// Creates an evaluator from injected schedule side-effect seams.
    #[must_use]
    pub fn new(
        timer: Arc<dyn ScheduleTimer>,
        starter: Arc<dyn ScheduleWorkflowStarter>,
        canceller: Arc<dyn ScheduleWorkflowCanceller>,
        events: Arc<dyn ScheduleEventSink>,
    ) -> Self {
        Self {
            states: HashMap::new(),
            pending_ticks: HashSet::new(),
            timer,
            starter,
            canceller,
            events,
        }
    }

    /// Replaces evaluator state with a projection from schedule events.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleEvaluatorError`] when projection trigger evaluation fails.
    pub fn replace_state_from_events(
        &mut self,
        events: &[Event],
    ) -> Result<(), ScheduleEvaluatorError> {
        self.states = project_schedule_state(events)?
            .into_iter()
            .map(|state| (state.schedule_id.clone(), state))
            .collect();
        self.pending_ticks.clear();
        Ok(())
    }

    /// Inserts or replaces a schedule state. Primarily useful for tests and future Engine wiring.
    pub fn upsert_state(&mut self, state: ScheduleState) {
        let schedule_id = state.schedule_id.clone();
        if !state.is_active() {
            self.pending_ticks.remove(&schedule_id);
        }
        self.states.insert(schedule_id, state);
    }

    /// Returns the projected state for a schedule, when present.
    #[must_use]
    pub fn state(&self, schedule_id: &ScheduleId) -> Option<&ScheduleState> {
        self.states.get(schedule_id)
    }

    /// Returns all projected schedule states.
    pub fn states(&self) -> impl Iterator<Item = &ScheduleState> {
        self.states.values()
    }

    /// Returns whether one buffered tick is pending for a schedule.
    #[must_use]
    pub fn has_pending_tick(&self, schedule_id: &ScheduleId) -> bool {
        self.pending_ticks.contains(schedule_id)
    }

    /// Computes a durable timer id for a schedule timer.
    ///
    /// # Errors
    ///
    /// Returns [`aion_core::IdError`] only if the generated name is empty, which cannot occur for a
    /// valid [`ScheduleId`]. The error is still propagated instead of panicking.
    pub fn timer_id_for(schedule_id: &ScheduleId) -> Result<TimerId, aion_core::IdError> {
        TimerId::named(format!("schedule:{schedule_id}"))
    }

    /// Arms an active schedule at its projected next trigger time.
    ///
    /// Paused and deleted schedules are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleEvaluatorError`] when timer id construction or durable arming fails.
    pub async fn arm_active_schedule(
        &self,
        schedule_id: &ScheduleId,
    ) -> Result<bool, ScheduleEvaluatorError> {
        let Some(state) = self.states.get(schedule_id) else {
            return Err(ScheduleEvaluatorError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            });
        };

        self.arm_state_if_active(state).await
    }

    async fn arm_state_if_active(
        &self,
        state: &ScheduleState,
    ) -> Result<bool, ScheduleEvaluatorError> {
        if !state.is_active() {
            return Ok(false);
        }

        let timer_id = Self::timer_id_for(&state.schedule_id)?;
        self.timer
            .arm_schedule_timer(&state.schedule_id, &timer_id, state.next_trigger_at)
            .await?;
        Ok(true)
    }

    /// Handles a fired schedule timer without polling.
    ///
    /// The timer owner calls this method when a durable schedule timer fires. The evaluator enforces
    /// overlap policy, records `ScheduleTriggered` after successful starts, computes the next fire
    /// time, and re-arms active schedules.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleEvaluatorError`] when policy side effects, event recording, trigger
    /// advancement, or durable timer arming fail.
    pub async fn handle_timer_fired(
        &mut self,
        schedule_id: &ScheduleId,
        fire_at: DateTime<Utc>,
    ) -> Result<TimerEvaluationOutcome, ScheduleEvaluatorError> {
        if !self
            .states
            .get(schedule_id)
            .ok_or_else(|| ScheduleEvaluatorError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            })?
            .is_active()
        {
            return Ok(TimerEvaluationOutcome::Inactive);
        }

        let outcome = self.process_fire(schedule_id, fire_at).await?;
        self.advance_and_arm(schedule_id, fire_at).await?;
        Ok(outcome)
    }

    async fn process_fire(
        &mut self,
        schedule_id: &ScheduleId,
        fire_at: DateTime<Utc>,
    ) -> Result<TimerEvaluationOutcome, ScheduleEvaluatorError> {
        let decision = {
            let state = self.states.get(schedule_id).ok_or_else(|| {
                ScheduleEvaluatorError::ScheduleNotFound {
                    schedule_id: schedule_id.clone(),
                }
            })?;
            evaluate_overlap(
                &state.config.overlap_policy,
                state.current_execution.as_ref(),
                self.pending_ticks.contains(schedule_id),
            )
        };

        match decision {
            OverlapDecision::Start => self.start_and_record(schedule_id, fire_at).await,
            OverlapDecision::Skip => Ok(TimerEvaluationOutcome::Skipped),
            OverlapDecision::BufferPending => {
                self.pending_ticks.insert(schedule_id.clone());
                Ok(TimerEvaluationOutcome::Buffered)
            }
            OverlapDecision::CancelThenStart(execution) => {
                self.canceller
                    .cancel_scheduled_workflow(&execution, "schedule overlap policy CancelPrevious")
                    .await?;
                if let Some(state) = self.states.get_mut(schedule_id) {
                    state.current_execution = None;
                }
                self.start_and_record(schedule_id, fire_at).await
            }
        }
    }

    async fn start_and_record(
        &mut self,
        schedule_id: &ScheduleId,
        recorded_at: DateTime<Utc>,
    ) -> Result<TimerEvaluationOutcome, ScheduleEvaluatorError> {
        let (workflow_type, input, search_attributes) = {
            let state = self.states.get(schedule_id).ok_or_else(|| {
                ScheduleEvaluatorError::ScheduleNotFound {
                    schedule_id: schedule_id.clone(),
                }
            })?;
            (
                state.config.workflow_type.clone(),
                state.config.input.clone(),
                state.config.search_attributes.clone(),
            )
        };

        let execution = self
            .starter
            .start_scheduled_workflow(&workflow_type, input, search_attributes)
            .await?;
        self.events
            .record_schedule_triggered(schedule_id, &execution, recorded_at)
            .await?;

        let state = self.states.get_mut(schedule_id).ok_or_else(|| {
            ScheduleEvaluatorError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            }
        })?;
        state.record_triggered(execution.clone(), recorded_at);
        Ok(TimerEvaluationOutcome::Started(execution))
    }

    async fn advance_and_arm(
        &mut self,
        schedule_id: &ScheduleId,
        after: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError> {
        let state = self.states.get_mut(schedule_id).ok_or_else(|| {
            ScheduleEvaluatorError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            }
        })?;
        let next = next_fire_time(&state.config.trigger, after)?;
        state.set_next_trigger_at(next);
        let state_snapshot = state.clone();
        self.arm_state_if_active(&state_snapshot).await?;
        Ok(())
    }

    /// Reconstructs schedule state from a recovery source, applies catch-up policy, and re-arms
    /// active schedules.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleEvaluatorError`] when source reading, projection, catch-up starts/event
    /// recording, trigger advancement, or durable timer arming fails.
    pub async fn recover_on_startup(
        &mut self,
        source: &dyn ScheduleEventSource,
        now: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError> {
        let events = source.schedule_events().await?;
        self.replace_state_from_events(&events)?;
        self.recover_projected_state(now).await
    }

    /// Applies catch-up policy to current projected state and arms active schedules.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleEvaluatorError`] when catch-up or timer side effects fail.
    pub async fn recover_projected_state(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError> {
        let schedule_ids = self.states.keys().cloned().collect::<Vec<_>>();
        for schedule_id in schedule_ids {
            let Some(state) = self.states.get(&schedule_id) else {
                continue;
            };
            if !state.is_active() {
                continue;
            }
            if let Some(state) = self.states.get_mut(&schedule_id) {
                state.current_execution = None;
            }
            let Some(state) = self.states.get(&schedule_id) else {
                continue;
            };

            let plan = evaluate_catch_up(
                &state.config.catch_up_policy,
                &state.config.trigger,
                state.next_trigger_at,
                now,
            )?;

            for fire_time in plan.fire_times {
                self.process_fire(&schedule_id, fire_time).await?;
            }

            if let Some(state) = self.states.get_mut(&schedule_id) {
                state.set_next_trigger_at(plan.next_trigger_at);
            }
            self.arm_active_schedule(&schedule_id).await?;
        }
        Ok(())
    }

    /// Clears the current execution and, if `BufferOne` queued a tick, starts exactly one buffered
    /// execution immediately.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleEvaluatorError`] when starting or event recording the buffered tick fails.
    pub async fn complete_current_execution(
        &mut self,
        schedule_id: &ScheduleId,
        completed_at: DateTime<Utc>,
    ) -> Result<Option<ScheduleExecution>, ScheduleEvaluatorError> {
        let state = self.states.get_mut(schedule_id).ok_or_else(|| {
            ScheduleEvaluatorError::ScheduleNotFound {
                schedule_id: schedule_id.clone(),
            }
        })?;
        state.current_execution = None;
        if !state.is_active() {
            self.pending_ticks.remove(schedule_id);
            return Ok(None);
        }

        if !self.pending_ticks.remove(schedule_id) {
            return Ok(None);
        }

        match self.start_and_record(schedule_id, completed_at).await? {
            TimerEvaluationOutcome::Started(execution) => Ok(Some(execution)),
            TimerEvaluationOutcome::Skipped
            | TimerEvaluationOutcome::Buffered
            | TimerEvaluationOutcome::Inactive => Err(ScheduleEvaluatorError::side_effect(
                "buffered schedule tick did not start a workflow",
            )),
        }
    }
}

/// A no-op canceller for evaluators that do not need `CancelPrevious` support in a given test seam.
pub struct NoopScheduleCanceller;

#[async_trait]
impl ScheduleWorkflowCanceller for NoopScheduleCanceller {
    async fn cancel_scheduled_workflow(
        &self,
        _execution: &ScheduleExecution,
        _reason: &str,
    ) -> Result<(), ScheduleEvaluatorError> {
        Ok(())
    }
}

/// Durable timer adapter backed by the workflow timer store API with a schedule coordinator owner.
pub struct StoreScheduleTimer<S: ?Sized> {
    store: Arc<S>,
    coordinator_workflow_id: WorkflowId,
}

impl<S: ?Sized> StoreScheduleTimer<S> {
    /// Creates a store-backed schedule timer adapter.
    #[must_use]
    pub fn new(store: Arc<S>, coordinator_workflow_id: WorkflowId) -> Self {
        Self {
            store,
            coordinator_workflow_id,
        }
    }
}

#[async_trait]
impl<S> ScheduleTimer for StoreScheduleTimer<S>
where
    S: aion_store::EventStore + ?Sized,
{
    async fn arm_schedule_timer(
        &self,
        _schedule_id: &ScheduleId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), ScheduleEvaluatorError> {
        self.store
            .schedule_timer(&self.coordinator_workflow_id, timer_id, fire_at)
            .await
            .map_err(|error| ScheduleEvaluatorError::side_effect(error.to_string()))
    }
}
