//! Schedule state projection from durable schedule events.

use aion_core::{Event, RunId, ScheduleConfig, ScheduleId, WorkflowId};
use chrono::{DateTime, Utc};

use crate::schedule::{ScheduleError, next_fire_time};

/// Workflow execution most recently started by a schedule tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleExecution {
    /// Workflow execution identifier started by the schedule.
    pub workflow_id: WorkflowId,
    /// Concrete run identifier started by the schedule.
    pub run_id: RunId,
}

impl ScheduleExecution {
    /// Creates a projected schedule execution reference.
    #[must_use]
    pub const fn new(workflow_id: WorkflowId, run_id: RunId) -> Self {
        Self {
            workflow_id,
            run_id,
        }
    }
}

/// Per-schedule projection derived from schedule events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleState {
    /// Schedule resource identifier.
    pub schedule_id: ScheduleId,
    /// Current schedule configuration.
    pub config: ScheduleConfig,
    /// Whether the schedule is paused.
    pub is_paused: bool,
    /// Whether the schedule has been deleted.
    pub is_deleted: bool,
    /// Timestamp of the latest recorded schedule trigger.
    pub last_triggered_at: Option<DateTime<Utc>>,
    /// Next eligible trigger timestamp, when one can be computed.
    pub next_trigger_at: DateTime<Utc>,
    /// Latest workflow execution started by this schedule, if any.
    pub current_execution: Option<ScheduleExecution>,
}

impl ScheduleState {
    /// Builds initial state from a `ScheduleCreated` event payload.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when the trigger cannot produce an initial next fire time.
    pub fn created(
        schedule_id: ScheduleId,
        config: ScheduleConfig,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, ScheduleError> {
        let next_trigger_at = next_fire_time(&config.trigger, recorded_at)?;
        Ok(Self {
            schedule_id,
            config,
            is_paused: false,
            is_deleted: false,
            last_triggered_at: None,
            next_trigger_at,
            current_execution: None,
        })
    }

    /// Applies a schedule event to this projection.
    ///
    /// Non-schedule events and events for other schedules are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when an event changes configuration or advances a trigger and the
    /// trigger cannot produce a next fire time.
    pub fn apply(&mut self, event: &Event) -> Result<(), ScheduleError> {
        match event {
            Event::ScheduleCreated {
                schedule_id,
                config,
                envelope,
            }
            | Event::ScheduleUpdated {
                schedule_id,
                config,
                envelope,
            } if schedule_id == &self.schedule_id => {
                self.config = config.clone();
                self.next_trigger_at = next_fire_time(&self.config.trigger, envelope.recorded_at)?;
            }
            Event::SchedulePaused { schedule_id, .. } if schedule_id == &self.schedule_id => {
                self.is_paused = true;
            }
            Event::ScheduleResumed {
                schedule_id,
                envelope,
            } if schedule_id == &self.schedule_id => {
                self.is_paused = false;
                self.next_trigger_at = next_fire_time(&self.config.trigger, envelope.recorded_at)?;
            }
            Event::ScheduleDeleted { schedule_id, .. } if schedule_id == &self.schedule_id => {
                self.is_deleted = true;
            }
            Event::ScheduleTriggered {
                schedule_id,
                workflow_id,
                run_id,
                envelope,
            } if schedule_id == &self.schedule_id => {
                self.last_triggered_at = Some(envelope.recorded_at);
                self.current_execution =
                    Some(ScheduleExecution::new(workflow_id.clone(), run_id.clone()));
                self.next_trigger_at = next_fire_time(&self.config.trigger, envelope.recorded_at)?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Returns whether this schedule should have an armed timer.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        !self.is_paused && !self.is_deleted
    }

    /// Replaces the next trigger timestamp after evaluator-side catch-up or re-arm calculation.
    pub const fn set_next_trigger_at(&mut self, next_trigger_at: DateTime<Utc>) {
        self.next_trigger_at = next_trigger_at;
    }

    /// Records that the evaluator started a workflow for this schedule.
    pub fn record_triggered(&mut self, execution: ScheduleExecution, recorded_at: DateTime<Utc>) {
        self.last_triggered_at = Some(recorded_at);
        self.current_execution = Some(execution);
    }
}

/// Projects schedule state from an ordered event history.
///
/// # Errors
///
/// Returns [`ScheduleError`] when a schedule trigger cannot be evaluated while applying events.
pub fn project_schedule_state(events: &[Event]) -> Result<Vec<ScheduleState>, ScheduleError> {
    let mut states = Vec::<ScheduleState>::new();

    for event in events {
        if let Event::ScheduleCreated {
            schedule_id,
            config,
            envelope,
        } = event
        {
            if !states.iter().any(|state| &state.schedule_id == schedule_id) {
                states.push(ScheduleState::created(
                    schedule_id.clone(),
                    config.clone(),
                    envelope.recorded_at,
                )?);
                continue;
            }
        }

        for state in &mut states {
            state.apply(event)?;
        }
    }

    Ok(states)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aion_core::{
        CatchUpPolicy, EventEnvelope, OverlapPolicy, Payload, ScheduleConfig, TriggerSpec,
    };
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::*;

    fn parse_utc(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
        DateTime::parse_from_rfc3339(value).map(|date_time| date_time.with_timezone(&Utc))
    }

    fn config(label: &str, period_secs: u64) -> Result<ScheduleConfig, aion_core::PayloadError> {
        Ok(ScheduleConfig {
            trigger: TriggerSpec::Interval {
                period: Duration::from_secs(period_secs),
            },
            overlap_policy: OverlapPolicy::Skip,
            catch_up_policy: CatchUpPolicy::One,
            workflow_type: String::from("checkout"),
            input: Payload::from_json(&json!({ "label": label }))?,
        })
    }

    fn envelope(seq: u64, recorded_at: DateTime<Utc>) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at,
            workflow_id: WorkflowId::new_v4(),
        }
    }

    #[test]
    fn schedule_events_project_state_fields() -> Result<(), Box<dyn std::error::Error>> {
        let schedule_id = ScheduleId::new_v4();
        let created_at = parse_utc("2026-06-07T00:00:00Z")?;
        let updated_at = parse_utc("2026-06-07T00:01:00Z")?;
        let triggered_at = parse_utc("2026-06-07T00:02:00Z")?;
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();

        let created_config = config("created", 60)?;
        let updated_config = config("updated", 120)?;
        let events = vec![
            Event::ScheduleCreated {
                envelope: envelope(1, created_at),
                schedule_id: schedule_id.clone(),
                config: created_config,
            },
            Event::ScheduleUpdated {
                envelope: envelope(2, updated_at),
                schedule_id: schedule_id.clone(),
                config: updated_config.clone(),
            },
            Event::SchedulePaused {
                envelope: envelope(3, updated_at),
                schedule_id: schedule_id.clone(),
            },
            Event::ScheduleResumed {
                envelope: envelope(4, updated_at),
                schedule_id: schedule_id.clone(),
            },
            Event::ScheduleTriggered {
                envelope: envelope(5, triggered_at),
                schedule_id: schedule_id.clone(),
                workflow_id: workflow_id.clone(),
                run_id: run_id.clone(),
            },
            Event::ScheduleDeleted {
                envelope: envelope(6, triggered_at),
                schedule_id: schedule_id.clone(),
            },
        ];

        let projected = project_schedule_state(&events)?;
        let state = projected
            .iter()
            .find(|state| state.schedule_id == schedule_id)
            .ok_or("missing projected schedule")?;

        assert_eq!(state.config, updated_config);
        assert!(!state.is_paused);
        assert!(state.is_deleted);
        assert_eq!(state.last_triggered_at, Some(triggered_at));
        assert_eq!(
            state.current_execution,
            Some(ScheduleExecution::new(workflow_id, run_id))
        );
        assert_eq!(
            state.next_trigger_at,
            triggered_at + chrono::Duration::seconds(120)
        );
        Ok(())
    }

    #[test]
    fn created_state_is_unpaused_with_initial_next_trigger()
    -> Result<(), Box<dyn std::error::Error>> {
        let recorded_at = parse_utc("2026-06-07T00:00:00Z")?;
        let state =
            ScheduleState::created(ScheduleId::new_v4(), config("created", 30)?, recorded_at)?;

        assert!(!state.is_paused);
        assert!(!state.is_deleted);
        assert_eq!(
            state.next_trigger_at,
            recorded_at + chrono::Duration::seconds(30)
        );
        Ok(())
    }
}
