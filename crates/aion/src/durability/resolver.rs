//! Resolver: recorded/resume-live/violation decision.

use aion_core::{Event, WorkflowId};

use crate::durability::{
    Command, CorrelationKey, CursorResolveResult, DurabilityError, HistoryCursor,
    NonDeterminismError, RecordedEventFamily, Resolution, ResolveOutcome,
    cursor::FoundEventDescriptor,
};

/// Single durability chokepoint that resolves workflow commands against recorded history.
#[derive(Clone, Debug)]
pub struct Resolver {
    workflow_id: WorkflowId,
    cursor: HistoryCursor,
}

impl Resolver {
    /// Creates a resolver for one workflow history.
    ///
    /// The workflow id is retained for typed non-determinism diagnostics; AD-006 wires the
    /// determinism-context timestamp hook at this same chokepoint.
    #[must_use]
    pub const fn new(workflow_id: WorkflowId, cursor: HistoryCursor) -> Self {
        Self {
            workflow_id,
            cursor,
        }
    }

    /// Resolves a command from recorded history or returns [`ResolveOutcome::ResumeLive`].
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError::NonDeterminism`] when the cursor reports a command-stream
    /// mismatch, or [`DurabilityError::HistoryShape`] when matched history lacks one of AD-004's
    /// recorded terminal outcomes.
    pub fn resolve(&mut self, command: Command) -> Result<ResolveOutcome, DurabilityError> {
        let Some((family, key)) = family_and_key(&command) else {
            return Ok(ResolveOutcome::ResumeLive);
        };

        match self.cursor.resolve_next(family, key) {
            CursorResolveResult::Matched(events) => resolution_from_matched(&events),
            CursorResolveResult::Exhausted => Ok(ResolveOutcome::ResumeLive),
            CursorResolveResult::Mismatch {
                expected_key,
                found,
            } => Err(self.mismatch_error(family, expected_key, found).into()),
        }
    }

    fn mismatch_error(
        &self,
        expected_family: RecordedEventFamily,
        expected_key: CorrelationKey,
        found: FoundEventDescriptor,
    ) -> NonDeterminismError {
        NonDeterminismError {
            workflow_id: self.workflow_id.clone(),
            seq: found.seq,
            expected: format!("{expected_family:?} {expected_key:?}"),
            found: format!(
                "{} family {:?} key {:?}",
                found.kind, found.family, found.key
            ),
        }
    }
}

fn family_and_key(command: &Command) -> Option<(RecordedEventFamily, CorrelationKey)> {
    match command {
        Command::RunActivity { key, .. } => Some((RecordedEventFamily::Activity, key.clone())),
        Command::AwaitSignal { key } => Some((RecordedEventFamily::Signal, key.clone())),
        Command::StartTimer { key, .. } => Some((RecordedEventFamily::Timer, key.clone())),
        Command::SpawnChild { key, .. } => Some((RecordedEventFamily::Child, key.clone())),
        Command::CompleteWorkflow { .. } => None,
    }
}

fn resolution_from_matched(events: &[Event]) -> Result<ResolveOutcome, DurabilityError> {
    let Some(last) = events.last() else {
        return Err(DurabilityError::HistoryShape {
            reason: "cursor returned an empty matched event range".to_owned(),
        });
    };

    match last {
        Event::ActivityCompleted { result, .. } => {
            Ok(recorded(Resolution::ActivityCompleted(result.clone())))
        }
        Event::ActivityFailed { error, .. } => {
            Ok(recorded(Resolution::ActivityFailedTerminal(error.clone())))
        }
        Event::TimerFired { .. } => Ok(recorded(Resolution::TimerFired)),
        Event::SignalReceived { payload, .. } => {
            Ok(recorded(Resolution::SignalDelivered(payload.clone())))
        }
        Event::ChildWorkflowCompleted { result, .. } => {
            Ok(recorded(Resolution::ChildCompleted(result.clone())))
        }
        Event::ChildWorkflowFailed { error, .. } => {
            Ok(recorded(Resolution::ChildFailed(error.clone())))
        }
        Event::TimerStarted { .. } | Event::ChildWorkflowStarted { .. } => {
            Ok(ResolveOutcome::ResumeLive)
        }
        Event::ActivityCancelled { .. }
        | Event::TimerCancelled { .. }
        | Event::ChildWorkflowCancelled { .. } => Err(DurabilityError::HistoryShape {
            reason: format!(
                "recorded cancellation outcome is not representable by AD-004 resolution: {}",
                event_kind(last)
            ),
        }),
        Event::WorkflowStarted { .. }
        | Event::WorkflowCompleted { .. }
        | Event::WorkflowFailed { .. }
        | Event::WorkflowCancelled { .. }
        | Event::WorkflowTimedOut { .. }
        | Event::ActivityScheduled { .. }
        | Event::ActivityStarted { .. } => Err(DurabilityError::HistoryShape {
            reason: format!(
                "matched history ended without a recorded command outcome: {}",
                event_kind(last)
            ),
        }),
    }
}

fn recorded(resolution: Resolution) -> ResolveOutcome {
    ResolveOutcome::Recorded(resolution)
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::WorkflowStarted { .. } => "WorkflowStarted",
        Event::WorkflowCompleted { .. } => "WorkflowCompleted",
        Event::WorkflowFailed { .. } => "WorkflowFailed",
        Event::WorkflowCancelled { .. } => "WorkflowCancelled",
        Event::WorkflowTimedOut { .. } => "WorkflowTimedOut",
        Event::ActivityScheduled { .. } => "ActivityScheduled",
        Event::ActivityStarted { .. } => "ActivityStarted",
        Event::ActivityCompleted { .. } => "ActivityCompleted",
        Event::ActivityFailed { .. } => "ActivityFailed",
        Event::ActivityCancelled { .. } => "ActivityCancelled",
        Event::TimerStarted { .. } => "TimerStarted",
        Event::TimerFired { .. } => "TimerFired",
        Event::TimerCancelled { .. } => "TimerCancelled",
        Event::SignalReceived { .. } => "SignalReceived",
        Event::ChildWorkflowStarted { .. } => "ChildWorkflowStarted",
        Event::ChildWorkflowCompleted { .. } => "ChildWorkflowCompleted",
        Event::ChildWorkflowFailed { .. } => "ChildWorkflowFailed",
        Event::ChildWorkflowCancelled { .. } => "ChildWorkflowCancelled",
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{
        ActivityError, ActivityErrorKind, ActivityId, Event, EventEnvelope, Payload, TimerId,
        WorkflowError, WorkflowId,
    };
    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::Resolver;
    use crate::durability::{Command, CorrelationKey, HistoryCursor, Resolution, ResolveOutcome};

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(Uuid::nil())
    }

    fn child_workflow_id() -> WorkflowId {
        WorkflowId::new(Uuid::from_u128(1))
    }

    fn timestamp() -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        Utc.timestamp_opt(0, 0)
            .single()
            .ok_or_else(|| "invalid timestamp".into())
    }

    fn envelope(seq: u64) -> Result<EventEnvelope, Box<dyn std::error::Error>> {
        Ok(EventEnvelope {
            seq,
            recorded_at: timestamp()?,
            workflow_id: workflow_id(),
        })
    }

    fn payload(label: &str) -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&json!({ "label": label }))?)
    }

    fn workflow_error(message: &str) -> WorkflowError {
        WorkflowError {
            message: message.to_owned(),
            details: None,
        }
    }

    fn activity_scheduled(seq: u64, ordinal: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityScheduled {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            activity_type: "activity".to_owned(),
            input: payload("activity-input")?,
        })
    }

    fn activity_completed(
        seq: u64,
        ordinal: u64,
        result: Payload,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityCompleted {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            result,
        })
    }

    fn timer_started(seq: u64, timer_id: TimerId) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::TimerStarted {
            envelope: envelope(seq)?,
            timer_id,
            fire_at: timestamp()?,
        })
    }

    fn timer_fired(seq: u64, timer_id: TimerId) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::TimerFired {
            envelope: envelope(seq)?,
            timer_id,
        })
    }

    fn signal_received(
        seq: u64,
        name: &str,
        payload: Payload,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::SignalReceived {
            envelope: envelope(seq)?,
            name: name.to_owned(),
            payload,
        })
    }

    fn child_started(seq: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ChildWorkflowStarted {
            envelope: envelope(seq)?,
            child_workflow_id: child_workflow_id(),
            workflow_type: "child".to_owned(),
            input: payload("child-input")?,
        })
    }

    fn child_completed(seq: u64, result: Payload) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ChildWorkflowCompleted {
            envelope: envelope(seq)?,
            child_workflow_id: child_workflow_id(),
            result,
        })
    }

    fn run_activity_command(ordinal: u64) -> Result<Command, Box<dyn std::error::Error>> {
        Ok(Command::RunActivity {
            key: CorrelationKey::Activity(ordinal),
            activity_type: "activity".to_owned(),
            input: payload("activity-input")?,
        })
    }

    #[test]
    fn resolves_recorded_activity_then_resumes_live_at_history_end()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = payload("activity-result")?;
        let cursor = HistoryCursor::new(vec![
            activity_scheduled(1, 0)?,
            activity_completed(2, 0, result.clone())?,
        ])?;
        let mut resolver = Resolver::new(workflow_id(), cursor);

        assert_eq!(
            resolver.resolve(run_activity_command(0)?)?,
            ResolveOutcome::Recorded(Resolution::ActivityCompleted(result))
        );
        assert_eq!(
            resolver.resolve(run_activity_command(1)?)?,
            ResolveOutcome::ResumeLive
        );
        Ok(())
    }

    #[test]
    fn resolves_all_recorded_families_through_single_entry_point()
    -> Result<(), Box<dyn std::error::Error>> {
        let activity_result = payload("activity-result")?;
        let signal_payload = payload("signal-payload")?;
        let child_result = payload("child-result")?;
        let timer_id = TimerId::anonymous(9);
        let cursor = HistoryCursor::new(vec![
            activity_scheduled(1, 0)?,
            activity_completed(2, 0, activity_result.clone())?,
            timer_started(3, timer_id.clone())?,
            timer_fired(4, timer_id.clone())?,
            signal_received(5, "ready", signal_payload.clone())?,
            child_started(6)?,
            child_completed(7, child_result.clone())?,
        ])?;
        let mut resolver = Resolver::new(workflow_id(), cursor);

        assert_eq!(
            resolver.resolve(run_activity_command(0)?)?,
            ResolveOutcome::Recorded(Resolution::ActivityCompleted(activity_result))
        );
        assert_eq!(
            resolver.resolve(Command::StartTimer {
                key: CorrelationKey::Timer(timer_id),
                fire_at: timestamp()?,
            })?,
            ResolveOutcome::Recorded(Resolution::TimerFired)
        );
        assert_eq!(
            resolver.resolve(Command::AwaitSignal {
                key: CorrelationKey::Signal {
                    name: "ready".to_owned(),
                    index: 0,
                },
            })?,
            ResolveOutcome::Recorded(Resolution::SignalDelivered(signal_payload))
        );
        assert_eq!(
            resolver.resolve(Command::SpawnChild {
                key: CorrelationKey::Child(6),
                workflow_type: "child".to_owned(),
                input: payload("child-input")?,
            })?,
            ResolveOutcome::Recorded(Resolution::ChildCompleted(child_result))
        );
        Ok(())
    }

    #[test]
    fn maps_terminal_failures_to_recorded_resolutions() -> Result<(), Box<dyn std::error::Error>> {
        let activity_error = ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: "activity failed".to_owned(),
            details: None,
        };
        let child_error = workflow_error("child failed");
        let cursor = HistoryCursor::new(vec![
            activity_scheduled(1, 0)?,
            Event::ActivityFailed {
                envelope: envelope(2)?,
                activity_id: ActivityId::from_sequence_position(0),
                error: activity_error.clone(),
                attempt: 1,
            },
            child_started(3)?,
            Event::ChildWorkflowFailed {
                envelope: envelope(4)?,
                child_workflow_id: child_workflow_id(),
                error: child_error.clone(),
            },
        ])?;
        let mut resolver = Resolver::new(workflow_id(), cursor);

        assert_eq!(
            resolver.resolve(run_activity_command(0)?)?,
            ResolveOutcome::Recorded(Resolution::ActivityFailedTerminal(activity_error))
        );
        assert_eq!(
            resolver.resolve(Command::SpawnChild {
                key: CorrelationKey::Child(3),
                workflow_type: "child".to_owned(),
                input: payload("child-input")?,
            })?,
            ResolveOutcome::Recorded(Resolution::ChildFailed(child_error))
        );
        Ok(())
    }
}
