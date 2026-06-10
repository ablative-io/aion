//! `HistoryCursor` over recorded events.

use aion_core::{ActivityId, Event, RunId, WorkflowId};

use crate::durability::{
    correlation::{CorrelationKey, key_for_event},
    error::DurabilityError,
};

/// Slice an ordered multi-run history down to the segment for `run_id`.
///
/// continue-as-new appends each replacement run's events to the same
/// workflow history, but correlation identities (activity and timer
/// ordinals, per-name signal occurrence indices) are run-scoped: every
/// run's deterministic counters restart from zero. Replay and live command
/// resolution must therefore only see events recorded at or after the run's
/// own `WorkflowStarted`, or a replacement run would match the prior run's
/// recorded commands.
///
/// # Errors
///
/// Returns [`DurabilityError::HistoryShape`] when the history holds no
/// `WorkflowStarted` for `run_id`.
pub fn current_run_segment(
    history: Vec<Event>,
    run_id: &RunId,
) -> Result<Vec<Event>, DurabilityError> {
    let start = history
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::WorkflowStarted {
                    run_id: event_run_id,
                    ..
                } if event_run_id == run_id
            )
        })
        .ok_or_else(|| DurabilityError::HistoryShape {
            reason: format!("history has no WorkflowStarted for run {run_id}"),
        })?;
    let mut segment = history;
    segment.drain(..start);
    Ok(segment)
}

/// Event families that can satisfy world-touching workflow commands during replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordedEventFamily {
    /// Activity scheduling and its recorded outcome.
    Activity,
    /// Timer scheduling and its recorded outcome.
    Timer,
    /// Signal delivery.
    Signal,
    /// Child workflow scheduling and its recorded outcome.
    Child,
}

/// Data describing the recorded event found at the cursor's current position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundEventDescriptor {
    /// Sequence number of the recorded event.
    pub seq: u64,
    /// Replay family for the recorded event, if it is matchable by the cursor.
    pub family: Option<RecordedEventFamily>,
    /// Correlation key derived for the recorded event, if it starts a matchable command.
    pub key: Option<CorrelationKey>,
    /// Stable event-variant name for diagnostics.
    pub kind: &'static str,
}

/// Outcome of asking the cursor to resolve the next recorded command outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum CursorResolveResult {
    /// The recorded command stream matched; contained events were consumed in order.
    Matched(Vec<Event>),
    /// The cursor has no remaining recorded event to consider.
    Exhausted,
    /// The next recorded event exists, but its family or key differs from the command.
    Mismatch {
        /// Correlation key the workflow command expected to replay.
        expected_key: CorrelationKey,
        /// Descriptor for the recorded event actually at the cursor position.
        found: FoundEventDescriptor,
    },
}

/// Ordered cursor over a workflow's recorded history.
#[derive(Clone, Debug)]
pub struct HistoryCursor {
    events: Vec<Event>,
    position: usize,
}

impl HistoryCursor {
    /// Builds a cursor from ordered read-history output.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError::HistoryShape`] when event sequence numbers decrease.
    pub fn new(events: Vec<Event>) -> Result<Self, DurabilityError> {
        for pair in events.windows(2) {
            let prior = pair[0].seq();
            let next = pair[1].seq();
            if next < prior {
                return Err(DurabilityError::HistoryShape {
                    reason: format!("history sequence order decreased from {prior} to {next}"),
                });
            }
        }

        Ok(Self {
            events,
            position: 0,
        })
    }

    /// Returns the sequence number at the current cursor position, or `None` when exhausted.
    #[must_use]
    pub fn current_sequence(&self) -> Option<u64> {
        self.events.get(self.position).map(Event::seq)
    }

    /// Returns the ordered history backing this cursor.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Returns the current zero-based index into the owned history.
    #[must_use]
    pub const fn position_index(&self) -> usize {
        self.position
    }

    /// Returns the next matchable correlation key for `family` without consuming history.
    #[must_use]
    pub fn next_key(&self, family: RecordedEventFamily) -> Option<CorrelationKey> {
        let index = self.next_matchable_index()?;
        let descriptor = self.descriptor_at(index, &self.events[index]);
        if descriptor.family == Some(family) {
            descriptor.key
        } else {
            None
        }
    }

    /// Advance past recorded commands that earlier resolver instances of the
    /// same live execution already consumed.
    ///
    /// NIF calls each build a fresh resolver whose cursor starts at the top
    /// of history, so commands resolved by earlier calls sit before the one
    /// being resolved now. Skipping stops at the first matchable entry whose
    /// correlation key equals `key` — or at history end, leaving live
    /// resolution to proceed — so a genuinely out-of-order command for an
    /// already-positioned key still surfaces as a mismatch in
    /// [`HistoryCursor::resolve_next`]. Strict full-history replay
    /// (recovery, the replay driver) never calls this.
    pub fn fast_forward_to_key(&mut self, key: &CorrelationKey) {
        while let Some(index) = self.next_matchable_index() {
            let descriptor = self.descriptor_at(index, &self.events[index]);
            if descriptor.key.as_ref() == Some(key) {
                return;
            }
            self.position = index + 1;
        }
    }

    /// Resolves the next recorded child terminal outcome for `child_workflow_id`.
    #[must_use]
    pub fn resolve_child_terminal(
        &mut self,
        child_workflow_id: &WorkflowId,
    ) -> CursorResolveResult {
        let Some(found_index) = self.next_matchable_index() else {
            return CursorResolveResult::Exhausted;
        };
        self.position = found_index;
        match self.events.get(found_index) {
            Some(
                Event::ChildWorkflowCompleted {
                    child_workflow_id: completed_child,
                    ..
                }
                | Event::ChildWorkflowFailed {
                    child_workflow_id: completed_child,
                    ..
                },
            ) if completed_child == child_workflow_id => self.consume_one(),
            Some(event) => CursorResolveResult::Mismatch {
                expected_key: CorrelationKey::Child(0),
                found: self.descriptor_at(found_index, event),
            },
            None => CursorResolveResult::Exhausted,
        }
    }

    /// Resolves the next recorded outcome for the expected family and correlation key.
    #[must_use]
    pub fn resolve_next(
        &mut self,
        family: RecordedEventFamily,
        expected_key: CorrelationKey,
    ) -> CursorResolveResult {
        let Some(found_index) = self.next_matchable_index() else {
            return CursorResolveResult::Exhausted;
        };
        self.position = found_index;

        let found = self.descriptor_at(found_index, &self.events[found_index]);
        if found.family != Some(family) || found.key.as_ref() != Some(&expected_key) {
            return CursorResolveResult::Mismatch {
                expected_key,
                found,
            };
        }

        match family {
            RecordedEventFamily::Activity => self.resolve_activity(expected_key),
            RecordedEventFamily::Timer => {
                self.resolve_started_with_immediate_outcome(&expected_key)
            }
            RecordedEventFamily::Child => self.resolve_child(expected_key),
            RecordedEventFamily::Signal => match expected_key {
                CorrelationKey::Signal { .. } => self.consume_one(),
                _ => self.mismatch_at_current(expected_key),
            },
        }
    }

    fn resolve_child(&mut self, expected_key: CorrelationKey) -> CursorResolveResult {
        let CorrelationKey::Child(_) = expected_key else {
            return self.mismatch_at_current(expected_key);
        };

        match self.events.get(self.position) {
            Some(Event::ChildWorkflowStarted { .. }) => self.consume_one(),
            Some(
                Event::ChildWorkflowCompleted {
                    child_workflow_id, ..
                }
                | Event::ChildWorkflowFailed {
                    child_workflow_id, ..
                },
            ) if self.child_was_started(child_workflow_id) => self.consume_one(),
            Some(Event::ChildWorkflowCompleted { .. } | Event::ChildWorkflowFailed { .. }) => {
                CursorResolveResult::Exhausted
            }
            _ => self.mismatch_at_current(expected_key),
        }
    }

    fn child_was_started(&self, child_workflow_id: &WorkflowId) -> bool {
        self.events[..self.position].iter().any(|event| {
            matches!(
                event,
                Event::ChildWorkflowStarted {
                    child_workflow_id: started_child,
                    ..
                } if started_child == child_workflow_id
            )
        })
    }

    fn resolve_activity(&mut self, expected_key: CorrelationKey) -> CursorResolveResult {
        let Some(Event::ActivityScheduled { activity_id, .. }) = self.events.get(self.position)
        else {
            return self.mismatch_at_current(expected_key);
        };
        let activity_id = activity_id.clone();
        let start = self.position;
        let mut index = self.position + 1;

        while let Some(event) = self.events.get(index) {
            match event {
                Event::ActivityStarted {
                    activity_id: event_activity_id,
                    ..
                } if event_activity_id == &activity_id => {
                    index += 1;
                }
                Event::ActivityFailed {
                    activity_id: event_activity_id,
                    ..
                } if event_activity_id == &activity_id => {
                    if self.has_later_activity_attempt_or_outcome(index + 1, &activity_id) {
                        index += 1;
                    } else {
                        return self.consume_range(start, index + 1);
                    }
                }
                Event::ActivityCompleted {
                    activity_id: event_activity_id,
                    ..
                }
                | Event::ActivityCancelled {
                    activity_id: event_activity_id,
                    ..
                } if event_activity_id == &activity_id => {
                    return self.consume_range(start, index + 1);
                }
                _ => return self.mismatch_at_index(index, expected_key),
            }
        }

        CursorResolveResult::Exhausted
    }

    fn resolve_started_with_immediate_outcome(
        &mut self,
        expected_key: &CorrelationKey,
    ) -> CursorResolveResult {
        let start = self.position;
        let next = self.position + 1;
        if self
            .events
            .get(next)
            .is_some_and(|event| self.is_outcome_for_start_key(event, expected_key))
        {
            self.consume_range(start, next + 1)
        } else {
            self.consume_one()
        }
    }

    fn consume_one(&mut self) -> CursorResolveResult {
        self.consume_range(self.position, self.position + 1)
    }

    fn consume_range(&mut self, start: usize, end: usize) -> CursorResolveResult {
        let consumed = self.events[start..end].to_vec();
        self.position = end;
        CursorResolveResult::Matched(consumed)
    }

    fn next_matchable_index(&self) -> Option<usize> {
        self.events
            .iter()
            .enumerate()
            .skip(self.position)
            .find_map(|(index, event)| family_for_event(event).map(|_| index))
    }

    fn mismatch_at_current(&self, expected_key: CorrelationKey) -> CursorResolveResult {
        self.mismatch_at_index(self.position, expected_key)
    }

    fn mismatch_at_index(&self, index: usize, expected_key: CorrelationKey) -> CursorResolveResult {
        match self.events.get(index) {
            Some(event) => CursorResolveResult::Mismatch {
                expected_key,
                found: self.descriptor_at(index, event),
            },
            None => CursorResolveResult::Exhausted,
        }
    }

    fn descriptor_at(&self, index: usize, event: &Event) -> FoundEventDescriptor {
        FoundEventDescriptor {
            seq: event.seq(),
            family: family_for_event(event),
            key: key_for_event(&self.events, index),
            kind: event_kind(event),
        }
    }

    fn has_later_activity_attempt_or_outcome(
        &self,
        start: usize,
        activity_id: &ActivityId,
    ) -> bool {
        self.events.iter().skip(start).any(|event| {
            matches!(
                event,
                Event::ActivityStarted {
                    activity_id: event_activity_id,
                    ..
                } | Event::ActivityFailed {
                    activity_id: event_activity_id,
                    ..
                } | Event::ActivityCompleted {
                    activity_id: event_activity_id,
                    ..
                } if event_activity_id == activity_id
            )
        })
    }

    fn is_outcome_for_start_key(&self, event: &Event, expected_key: &CorrelationKey) -> bool {
        match (event, expected_key) {
            (
                Event::TimerFired { timer_id, .. }
                | Event::TimerCancelled { timer_id, .. }
                | Event::WithTimeoutCompleted { timer_id, .. },
                CorrelationKey::Timer(expected_timer_id),
            ) => timer_id == expected_timer_id,
            (
                Event::ChildWorkflowCompleted {
                    child_workflow_id, ..
                }
                | Event::ChildWorkflowFailed {
                    child_workflow_id, ..
                }
                | Event::ChildWorkflowCancelled {
                    child_workflow_id, ..
                },
                CorrelationKey::Child(_),
            ) => self.events.get(self.position).is_some_and(|start| {
                matches!(
                    start,
                    Event::ChildWorkflowStarted {
                        child_workflow_id: start_child_workflow_id,
                        ..
                    } if start_child_workflow_id == child_workflow_id
                )
            }),
            _ => false,
        }
    }
}

fn family_for_event(event: &Event) -> Option<RecordedEventFamily> {
    match event {
        Event::ActivityScheduled { .. } => Some(RecordedEventFamily::Activity),
        Event::TimerStarted { .. } | Event::WithTimeoutCompleted { .. } => {
            Some(RecordedEventFamily::Timer)
        }
        Event::SignalReceived { .. } | Event::SignalSent { .. } => {
            Some(RecordedEventFamily::Signal)
        }
        Event::ChildWorkflowStarted { .. }
        | Event::ChildWorkflowCompleted { .. }
        | Event::ChildWorkflowFailed { .. } => Some(RecordedEventFamily::Child),
        _ => None,
    }
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::WorkflowStarted { .. } => "WorkflowStarted",
        Event::WorkflowCompleted { .. } => "WorkflowCompleted",
        Event::WorkflowFailed { .. } => "WorkflowFailed",
        Event::WorkflowCancelled { .. } => "WorkflowCancelled",
        Event::WorkflowTimedOut { .. } => "WorkflowTimedOut",
        Event::WorkflowContinuedAsNew { .. } => "WorkflowContinuedAsNew",
        Event::SearchAttributesUpdated { .. } => "SearchAttributesUpdated",
        Event::ActivityScheduled { .. } => "ActivityScheduled",
        Event::ActivityStarted { .. } => "ActivityStarted",
        Event::ActivityCompleted { .. } => "ActivityCompleted",
        Event::ActivityFailed { .. } => "ActivityFailed",
        Event::ActivityCancelled { .. } => "ActivityCancelled",
        Event::TimerStarted { .. } => "TimerStarted",
        Event::TimerFired { .. } => "TimerFired",
        Event::TimerCancelled { .. } => "TimerCancelled",
        Event::WithTimeoutCompleted { .. } => "WithTimeoutCompleted",
        Event::SignalReceived { .. } => "SignalReceived",
        Event::SignalSent { .. } => "SignalSent",
        Event::ChildWorkflowStarted { .. } => "ChildWorkflowStarted",
        Event::ChildWorkflowCompleted { .. } => "ChildWorkflowCompleted",
        Event::ChildWorkflowFailed { .. } => "ChildWorkflowFailed",
        Event::ChildWorkflowCancelled { .. } => "ChildWorkflowCancelled",
        Event::ScheduleCreated { .. } => "ScheduleCreated",
        Event::ScheduleUpdated { .. } => "ScheduleUpdated",
        Event::SchedulePaused { .. } => "SchedulePaused",
        Event::ScheduleResumed { .. } => "ScheduleResumed",
        Event::ScheduleDeleted { .. } => "ScheduleDeleted",
        Event::ScheduleTriggered { .. } => "ScheduleTriggered",
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{
        ActivityError, ActivityErrorKind, ActivityId, Event, EventEnvelope, Payload, TimerId,
        WorkflowId,
    };
    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{CursorResolveResult, HistoryCursor, RecordedEventFamily};
    use crate::durability::correlation::CorrelationKey;

    fn timestamp() -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        Utc.timestamp_opt(0, 0)
            .single()
            .ok_or_else(|| "invalid timestamp".into())
    }

    fn envelope(seq: u64) -> Result<EventEnvelope, Box<dyn std::error::Error>> {
        Ok(EventEnvelope {
            seq,
            recorded_at: timestamp()?,
            workflow_id: WorkflowId::new(Uuid::nil()),
        })
    }

    fn payload() -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&json!(null))?)
    }

    fn workflow_started(seq: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(seq)?,
            workflow_type: "workflow".to_owned(),
            input: payload()?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
        })
    }

    fn scheduled(seq: u64, ordinal: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityScheduled {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            activity_type: "activity".to_owned(),
            input: payload()?,
        })
    }

    fn started(seq: u64, ordinal: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityStarted {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
        })
    }

    fn completed(seq: u64, ordinal: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityCompleted {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            result: payload()?,
        })
    }

    fn failed(
        seq: u64,
        ordinal: u64,
        attempt: u32,
        kind: ActivityErrorKind,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityFailed {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            error: ActivityError {
                kind,
                message: "activity failed".to_owned(),
                details: None,
            },
            attempt,
        })
    }

    #[test]
    fn new_accepts_in_order_history_and_exposes_starting_sequence()
    -> Result<(), Box<dyn std::error::Error>> {
        let cursor = HistoryCursor::new(vec![scheduled(7, 0)?, completed(8, 0)?])?;

        assert_eq!(cursor.current_sequence(), Some(7));
        assert_eq!(cursor.position_index(), 0);
        Ok(())
    }

    #[test]
    fn new_rejects_decreasing_sequence_order() -> Result<(), Box<dyn std::error::Error>> {
        let error = HistoryCursor::new(vec![scheduled(9, 0)?, completed(8, 0)?])
            .map(|_| "unexpected success")
            .err();

        assert!(error.is_some());
        Ok(())
    }

    #[test]
    fn resolves_activity_match_then_reports_exhaustion() -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![scheduled(1, 0)?, completed(2, 0)?])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 2);
                assert_eq!(cursor.current_sequence(), None);
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("activity should match recorded history".into());
            }
        }

        assert_eq!(
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(1),),
            CursorResolveResult::Exhausted
        );
        Ok(())
    }

    #[test]
    fn skips_non_matchable_lifecycle_events_before_resolving()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            workflow_started(1)?,
            scheduled(2, 0)?,
            completed(3, 0)?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 2);
                assert!(matches!(
                    events.first(),
                    Some(Event::ActivityScheduled { .. })
                ));
                assert_eq!(cursor.current_sequence(), None);
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("lifecycle events should not block command replay".into());
            }
        }
        Ok(())
    }

    #[test]
    fn reports_mismatch_for_different_next_family() -> Result<(), Box<dyn std::error::Error>> {
        let timer_id = TimerId::anonymous(1);
        let mut cursor = HistoryCursor::new(vec![Event::TimerStarted {
            envelope: envelope(1)?,
            timer_id: timer_id.clone(),
            fire_at: timestamp()?,
        }])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Mismatch {
                expected_key,
                found,
            } => {
                assert_eq!(expected_key, CorrelationKey::Activity(0));
                assert_eq!(found.family, Some(RecordedEventFamily::Timer));
                assert_eq!(found.key, Some(CorrelationKey::Timer(timer_id)));
            }
            CursorResolveResult::Matched(_) | CursorResolveResult::Exhausted => {
                return Err("different next family should be a mismatch".into());
            }
        }
        Ok(())
    }

    #[test]
    fn walks_retry_failures_to_eventual_activity_success() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            failed(2, 0, 1, ActivityErrorKind::Retryable)?,
            started(3, 0)?,
            failed(4, 0, 2, ActivityErrorKind::Retryable)?,
            completed(5, 0)?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 5);
                assert!(matches!(
                    events.last(),
                    Some(Event::ActivityCompleted { .. })
                ));
                assert_eq!(cursor.current_sequence(), None);
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("retry history should resolve to eventual completion".into());
            }
        }
        Ok(())
    }

    #[test]
    fn returns_terminal_activity_failure_as_recorded_outcome()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            failed(2, 0, 1, ActivityErrorKind::Retryable)?,
            failed(3, 0, 2, ActivityErrorKind::Terminal)?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 3);
                assert!(matches!(
                    events.last(),
                    Some(Event::ActivityFailed { error, .. }) if error.kind == ActivityErrorKind::Terminal
                ));
                assert_eq!(cursor.current_sequence(), None);
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("terminal failure should be the recorded outcome".into());
            }
        }
        Ok(())
    }
}
