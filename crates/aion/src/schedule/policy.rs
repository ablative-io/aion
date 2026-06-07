//! Pure schedule policy decisions for overlap and catch-up handling.

use aion_core::{CatchUpPolicy, OverlapPolicy, TriggerSpec};
use chrono::{DateTime, Utc};

use crate::schedule::{ScheduleError, ScheduleExecution, next_fire_time};

/// Decision produced when a schedule fires while another execution may be active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlapDecision {
    /// Start a new workflow execution immediately.
    Start,
    /// Skip this tick.
    Skip,
    /// Buffer one pending tick for later delivery.
    BufferPending,
    /// Cancel the current execution and then start a new one.
    CancelThenStart(ScheduleExecution),
}

/// Evaluates overlap policy from current execution and buffer state.
#[must_use]
pub fn evaluate_overlap(
    policy: &OverlapPolicy,
    current_execution: Option<&ScheduleExecution>,
    has_pending_tick: bool,
) -> OverlapDecision {
    match (policy, current_execution) {
        (_, None) | (OverlapPolicy::AllowAll, Some(_)) => OverlapDecision::Start,
        (OverlapPolicy::Skip, Some(_)) => OverlapDecision::Skip,
        (OverlapPolicy::BufferOne, Some(_)) if has_pending_tick => OverlapDecision::Skip,
        (OverlapPolicy::BufferOne, Some(_)) => OverlapDecision::BufferPending,
        (OverlapPolicy::CancelPrevious, Some(execution)) => {
            OverlapDecision::CancelThenStart(execution.clone())
        }
    }
}

/// Catch-up calculation result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatchUpPlan {
    /// Fire instants to process immediately during recovery.
    pub fire_times: Vec<DateTime<Utc>>,
    /// Future timestamp to arm after catch-up processing.
    pub next_trigger_at: DateTime<Utc>,
}

/// Evaluates catch-up policy when a schedule's projected next trigger is due or overdue.
///
/// # Errors
///
/// Returns [`ScheduleError`] when advancing the trigger fails.
pub fn evaluate_catch_up(
    policy: &CatchUpPolicy,
    trigger: &TriggerSpec,
    next_trigger_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<CatchUpPlan, ScheduleError> {
    if next_trigger_at > now {
        return Ok(CatchUpPlan {
            fire_times: Vec::new(),
            next_trigger_at,
        });
    }

    match policy {
        CatchUpPolicy::All => catch_up_all(trigger, next_trigger_at, now),
        CatchUpPolicy::One => catch_up_one(trigger, next_trigger_at, now),
        CatchUpPolicy::Skip => catch_up_skip(trigger, next_trigger_at, now),
    }
}

fn catch_up_all(
    trigger: &TriggerSpec,
    next_trigger_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<CatchUpPlan, ScheduleError> {
    let mut fire_times = Vec::new();
    let mut cursor = next_trigger_at;

    while cursor <= now {
        fire_times.push(cursor);
        cursor = next_fire_time(trigger, cursor)?;
    }

    Ok(CatchUpPlan {
        fire_times,
        next_trigger_at: cursor,
    })
}

fn catch_up_one(
    trigger: &TriggerSpec,
    next_trigger_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<CatchUpPlan, ScheduleError> {
    let mut cursor = next_fire_time(trigger, next_trigger_at)?;
    while cursor <= now {
        cursor = next_fire_time(trigger, cursor)?;
    }

    Ok(CatchUpPlan {
        fire_times: vec![next_trigger_at],
        next_trigger_at: cursor,
    })
}

fn catch_up_skip(
    trigger: &TriggerSpec,
    next_trigger_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<CatchUpPlan, ScheduleError> {
    let mut cursor = next_trigger_at;
    while cursor <= now {
        cursor = next_fire_time(trigger, cursor)?;
    }

    Ok(CatchUpPlan {
        fire_times: Vec::new(),
        next_trigger_at: cursor,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aion_core::{RunId, WorkflowId};
    use chrono::{DateTime, Utc};

    use super::*;

    fn parse_utc(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
        DateTime::parse_from_rfc3339(value).map(|date_time| date_time.with_timezone(&Utc))
    }

    fn execution() -> ScheduleExecution {
        ScheduleExecution::new(WorkflowId::new_v4(), RunId::new_v4())
    }

    fn trigger() -> TriggerSpec {
        TriggerSpec::Interval {
            period: Duration::from_secs(60),
        }
    }

    #[test]
    fn skip_policy_skips_when_execution_is_running() {
        assert_eq!(
            evaluate_overlap(&OverlapPolicy::Skip, Some(&execution()), false),
            OverlapDecision::Skip
        );
    }

    #[test]
    fn buffer_one_policy_queues_at_most_one_pending_tick() {
        let active = execution();
        assert_eq!(
            evaluate_overlap(&OverlapPolicy::BufferOne, Some(&active), false),
            OverlapDecision::BufferPending
        );
        assert_eq!(
            evaluate_overlap(&OverlapPolicy::BufferOne, Some(&active), true),
            OverlapDecision::Skip
        );
    }

    #[test]
    fn cancel_previous_policy_cancels_then_starts_when_execution_is_running() {
        let active = execution();
        assert_eq!(
            evaluate_overlap(&OverlapPolicy::CancelPrevious, Some(&active), false),
            OverlapDecision::CancelThenStart(active)
        );
    }

    #[test]
    fn allow_all_policy_starts_even_when_execution_is_running() {
        assert_eq!(
            evaluate_overlap(&OverlapPolicy::AllowAll, Some(&execution()), false),
            OverlapDecision::Start
        );
    }

    #[test]
    fn catch_up_all_fires_every_missed_trigger() -> Result<(), Box<dyn std::error::Error>> {
        let first = parse_utc("2026-06-07T00:01:00Z")?;
        let now = parse_utc("2026-06-07T00:03:00Z")?;
        let plan = evaluate_catch_up(&CatchUpPolicy::All, &trigger(), first, now)?;

        assert_eq!(
            plan.fire_times,
            vec![
                parse_utc("2026-06-07T00:01:00Z")?,
                parse_utc("2026-06-07T00:02:00Z")?,
                parse_utc("2026-06-07T00:03:00Z")?,
            ]
        );
        assert_eq!(plan.next_trigger_at, parse_utc("2026-06-07T00:04:00Z")?);
        Ok(())
    }

    #[test]
    fn catch_up_one_fires_once_and_advances_to_future() -> Result<(), Box<dyn std::error::Error>> {
        let first = parse_utc("2026-06-07T00:01:00Z")?;
        let now = parse_utc("2026-06-07T00:03:00Z")?;
        let plan = evaluate_catch_up(&CatchUpPolicy::One, &trigger(), first, now)?;

        assert_eq!(plan.fire_times, vec![first]);
        assert_eq!(plan.next_trigger_at, parse_utc("2026-06-07T00:04:00Z")?);
        Ok(())
    }

    #[test]
    fn catch_up_skip_only_advances_to_future() -> Result<(), Box<dyn std::error::Error>> {
        let first = parse_utc("2026-06-07T00:01:00Z")?;
        let now = parse_utc("2026-06-07T00:03:00Z")?;
        let plan = evaluate_catch_up(&CatchUpPolicy::Skip, &trigger(), first, now)?;

        assert!(plan.fire_times.is_empty());
        assert_eq!(plan.next_trigger_at, parse_utc("2026-06-07T00:04:00Z")?);
        Ok(())
    }
}
