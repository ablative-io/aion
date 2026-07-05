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

/// Outcome of asking the cursor for the recorded terminal outcome of one awaited child workflow.
///
/// `AwaitChild` is keyed by the child workflow id rather than a positional correlation key, so its
/// mismatch variant carries only the found-event descriptor; the resolver supplies the awaited
/// child identity in its diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub enum ChildTerminalResolveResult {
    /// The awaited child's recorded terminal event was consumed.
    Matched(Vec<Event>),
    /// The cursor has no remaining recorded event to consider.
    Exhausted,
    /// The next matchable recorded event is not the awaited child's terminal outcome.
    Mismatch {
        /// Descriptor for the recorded event actually at the cursor position.
        found: FoundEventDescriptor,
    },
}

/// Ordered cursor over a workflow's recorded history.
///
/// Commands consume only their own events. Asynchronous arrivals (signals,
/// child terminals, a parallel activity's events) can be recorded anywhere
/// inside another command's event range, so consumption is tracked per
/// event: a resolved command marks exactly its own events consumed and
/// skipped interior events stay matchable for their own commands.
/// `position` is the low-water mark — the first index that is neither
/// consumed nor already skipped past — and scans resume from it.
#[derive(Clone, Debug)]
pub struct HistoryCursor {
    events: Vec<Event>,
    consumed: Vec<bool>,
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

        let consumed = vec![false; events.len()];
        Ok(Self {
            events,
            consumed,
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

    /// Advance to the recorded terminal outcome for `child_workflow_id`.
    ///
    /// `AwaitChild` has no positional correlation key — its replay identity
    /// is the child workflow id returned by the matching spawn — so it gets
    /// the same skip treatment keyed commands receive from
    /// [`HistoryCursor::fast_forward_to_key`]: recorded commands consumed by
    /// earlier resolver instances of the same live execution are skipped
    /// until the awaited child's `ChildWorkflowCompleted`/`ChildWorkflowFailed`
    /// is reached. With no recorded terminal for that child the cursor
    /// exhausts, leaving resolution to hand off live. Strict full-history
    /// replay (recovery, the replay driver) never calls this.
    pub fn fast_forward_to_child_terminal(&mut self, child_workflow_id: &WorkflowId) {
        while let Some(index) = self.next_matchable_index() {
            if is_child_terminal_for(&self.events[index], child_workflow_id) {
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
    ) -> ChildTerminalResolveResult {
        let Some(found_index) = self.next_matchable_index() else {
            return ChildTerminalResolveResult::Exhausted;
        };
        self.position = found_index;
        match self.events.get(found_index) {
            Some(event) if is_child_terminal_for(event, child_workflow_id) => {
                ChildTerminalResolveResult::Matched(self.take_range(found_index, found_index + 1))
            }
            Some(event) => ChildTerminalResolveResult::Mismatch {
                found: self.descriptor_at(found_index, event),
            },
            None => ChildTerminalResolveResult::Exhausted,
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

    /// `resolve_next` only dispatches here once the found event's family is
    /// `Child` and its derived correlation key equals the expected key.
    /// Child terminal events carry no correlation key (`key_for_event` keys
    /// only `ChildWorkflowStarted`), so the position is provably at a
    /// `ChildWorkflowStarted`; the fallback mismatch arm guards the shape
    /// rather than encoding unreachable terminal handling.
    fn resolve_child(&mut self, expected_key: CorrelationKey) -> CursorResolveResult {
        match self.events.get(self.position) {
            Some(Event::ChildWorkflowStarted { .. }) => self.consume_one(),
            _ => self.mismatch_at_current(expected_key),
        }
    }

    /// Consumes the matched activity's `Scheduled -> terminal` events, keyed
    /// by activity id.
    ///
    /// Asynchronous arrivals (signals, child terminals, a parallel
    /// activity's events) can be recorded between this activity's
    /// `Scheduled` anchor and its terminal. They belong to other commands:
    /// the walk skips them in place — neither consuming them nor failing
    /// replay — leaving them matchable for their own commands. Determinism
    /// is enforced at the `Scheduled` anchor by `resolve_next`'s family/key
    /// equality check; a foreign interior event is an interleaving artifact,
    /// not a command-stream divergence.
    fn resolve_activity(&mut self, expected_key: CorrelationKey) -> CursorResolveResult {
        let Some(Event::ActivityScheduled { activity_id, .. }) = self.events.get(self.position)
        else {
            return self.mismatch_at_current(expected_key);
        };
        let activity_id = activity_id.clone();
        let mut matched = vec![self.position];
        let mut index = self.position + 1;

        while let Some(event) = self.events.get(index) {
            match event {
                Event::ActivityStarted {
                    activity_id: event_activity_id,
                    ..
                } if event_activity_id == &activity_id => {
                    matched.push(index);
                }
                Event::ActivityScheduled {
                    activity_id: event_activity_id,
                    ..
                } if event_activity_id == &activity_id => {
                    // A reopen re-dispatches the activity, recording a fresh
                    // ActivityScheduled for the same ordinal. Consume it as part
                    // of this activity's span so no straggler Scheduled is left
                    // matchable to mis-resolve a later command.
                    matched.push(index);
                }
                Event::ActivityFailed {
                    activity_id: event_activity_id,
                    ..
                } if event_activity_id == &activity_id => {
                    matched.push(index);
                    // A reopen supersedes this recorded failure: treat
                    // it like a non-terminal retry attempt and keep walking, so
                    // a later recorded attempt resolves the activity, or — with
                    // none yet — the walk exhausts and the activity re-dispatches
                    // live.
                    let superseded_by_reopen =
                        self.activity_reopened_after(index + 1, &activity_id);
                    if !superseded_by_reopen
                        && !self.has_later_activity_attempt_or_outcome(index + 1, &activity_id)
                    {
                        return self.consume_indices(matched);
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
                    matched.push(index);
                    return self.consume_indices(matched);
                }
                _ => {}
            }
            index += 1;
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
            CursorResolveResult::Matched(self.take_range(start, next + 1))
        } else {
            self.consume_one()
        }
    }

    fn consume_one(&mut self) -> CursorResolveResult {
        CursorResolveResult::Matched(self.take_range(self.position, self.position + 1))
    }

    fn take_range(&mut self, start: usize, end: usize) -> Vec<Event> {
        let consumed = self.events[start..end].to_vec();
        for slot in &mut self.consumed[start..end] {
            *slot = true;
        }
        self.advance_past_consumed();
        consumed
    }

    /// Marks exactly `indices` consumed and returns their events in order.
    ///
    /// Interior indices left unmarked stay matchable for their own commands;
    /// the position low-water mark advances only past the consumed prefix.
    fn consume_indices(&mut self, indices: Vec<usize>) -> CursorResolveResult {
        let mut events = Vec::with_capacity(indices.len());
        for index in indices {
            if let (Some(event), Some(slot)) =
                (self.events.get(index), self.consumed.get_mut(index))
            {
                *slot = true;
                events.push(event.clone());
            }
        }
        self.advance_past_consumed();
        CursorResolveResult::Matched(events)
    }

    fn advance_past_consumed(&mut self) {
        while self.consumed.get(self.position).copied().unwrap_or(false) {
            self.position += 1;
        }
    }

    fn next_matchable_index(&self) -> Option<usize> {
        let events = self.events.get(self.position..)?;
        let consumed = self.consumed.get(self.position..)?;
        events
            .iter()
            .zip(consumed)
            .position(|(event, consumed)| !consumed && family_for_event(event).is_some())
            .map(|offset| self.position + offset)
    }

    fn mismatch_at_current(&self, expected_key: CorrelationKey) -> CursorResolveResult {
        match self.events.get(self.position) {
            Some(event) => CursorResolveResult::Mismatch {
                expected_key,
                found: self.descriptor_at(self.position, event),
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

    /// Whether a later [`Event::WorkflowReopened`] names `activity_id` among its
    /// reopened activities. A reopen supersedes the activity's recorded terminal
    /// failure, so the walk treats that failure as a non-terminal attempt and
    /// continues — to a later recorded attempt, or to exhaustion so the activity
    /// re-dispatches live.
    fn activity_reopened_after(&self, start: usize, activity_id: &ActivityId) -> bool {
        self.events.iter().skip(start).any(|event| {
            matches!(
                event,
                Event::WorkflowReopened { reopened, .. } if reopened.contains(activity_id)
            )
        })
    }

    fn is_outcome_for_start_key(&self, event: &Event, expected_key: &CorrelationKey) -> bool {
        match (event, expected_key) {
            (
                Event::TimerFired { timer_id, .. } | Event::WithTimeoutCompleted { timer_id, .. },
                CorrelationKey::Timer(expected_timer_id),
            ) => timer_id == expected_timer_id,
            // A cancel-teardown TimerCancelled is engine bookkeeping (the run
            // was cancelled and its timers retired); it is NEVER an outcome a
            // workflow await observes — reopen re-arms the timer instead
            // (#222). Only a workflow-intent cancellation resolves the await.
            (
                Event::TimerCancelled {
                    timer_id, cause, ..
                },
                CorrelationKey::Timer(expected_timer_id),
            ) => {
                *cause == aion_core::TimerCancelCause::WorkflowIntent
                    && timer_id == expected_timer_id
            }
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

fn is_child_terminal_for(event: &Event, child_workflow_id: &WorkflowId) -> bool {
    matches!(
        event,
        Event::ChildWorkflowCompleted {
            child_workflow_id: terminal_child,
            ..
        } | Event::ChildWorkflowFailed {
            child_workflow_id: terminal_child,
            ..
        } if terminal_child == child_workflow_id
    )
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
        Event::WorkflowReopened { .. } => "WorkflowReopened",
        Event::WorkflowPaused { .. } => "WorkflowPaused",
        Event::WorkflowResumed { .. } => "WorkflowResumed",
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

    use super::{
        ChildTerminalResolveResult, CursorResolveResult, HistoryCursor, RecordedEventFamily,
    };
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
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn scheduled(seq: u64, ordinal: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityScheduled {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            activity_type: "activity".to_owned(),
            input: payload()?,
            task_queue: String::from("default"),
            node: None,
        })
    }

    fn started(seq: u64, ordinal: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityStarted {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            attempt: 1,
        })
    }

    fn completed(seq: u64, ordinal: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityCompleted {
            envelope: envelope(seq)?,
            activity_id: ActivityId::from_sequence_position(ordinal),
            result: payload()?,
            attempt: 1,
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

    fn reopened(seq: u64, reopened: &[u64]) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowReopened {
            envelope: envelope(seq)?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            reopened: reopened
                .iter()
                .map(|ordinal| ActivityId::from_sequence_position(*ordinal))
                .collect(),
        })
    }

    #[test]
    fn reopen_supersedes_terminal_failure_and_exhausts() -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            failed(2, 0, 1, ActivityErrorKind::Terminal)?,
            reopened(3, &[0])?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        assert!(
            matches!(result, CursorResolveResult::Exhausted),
            "a reopened terminal failure with no later attempt must resolve live, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn reopen_then_new_attempt_resolves_to_the_new_outcome()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            failed(2, 0, 1, ActivityErrorKind::Terminal)?,
            reopened(3, &[0])?,
            scheduled(4, 0)?,
            completed(5, 0)?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Matched(events) => {
                assert!(
                    matches!(events.last(), Some(Event::ActivityCompleted { .. })),
                    "reopened activity must resolve to its post-reopen completion"
                );
            }
            other => return Err(format!("expected Matched completion, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn terminal_failure_without_reopen_still_matches_unchanged()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            failed(2, 0, 1, ActivityErrorKind::Terminal)?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Matched(events) => {
                assert!(matches!(events.last(), Some(Event::ActivityFailed { .. })));
            }
            other => return Err(format!("expected Matched terminal failure, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn reopen_with_new_attempt_leaves_no_straggler_for_later_commands()
    -> Result<(), Box<dyn std::error::Error>> {
        // After a reopen re-dispatches activity 0 (a second Scheduled is
        // recorded), the next command (activity 1) must still resolve cleanly:
        // the post-reopen Scheduled(0) must not be left matchable.
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            failed(2, 0, 1, ActivityErrorKind::Terminal)?,
            reopened(3, &[0])?,
            scheduled(4, 0)?,
            completed(5, 0)?,
            scheduled(6, 1)?,
            completed(7, 1)?,
        ])?;

        let first = cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));
        assert!(
            matches!(first, CursorResolveResult::Matched(_)),
            "reopened activity 0 resolves, got {first:?}"
        );

        let second =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(1));
        match second {
            CursorResolveResult::Matched(events) => {
                assert!(
                    matches!(events.last(), Some(Event::ActivityCompleted { .. })),
                    "activity 1 must resolve to its completion after a reopen of activity 0"
                );
            }
            other => {
                return Err(format!(
                    "activity 1 must resolve cleanly after a reopen, got {other:?}"
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn reopen_of_a_different_activity_does_not_supersede_this_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            failed(2, 0, 1, ActivityErrorKind::Terminal)?,
            reopened(3, &[5])?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        assert!(
            matches!(result, CursorResolveResult::Matched(_)),
            "a reopen naming another activity must not reopen this one, got {result:?}"
        );
        Ok(())
    }

    fn paused(seq: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowPaused {
            envelope: envelope(seq)?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            reason: None,
            operator: None,
        })
    }

    fn resumed(seq: u64) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowResumed {
            envelope: envelope(seq)?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            operator: None,
        })
    }

    /// GATE-6 replay-invisibility (following the TimerCancelled-cause / reopen
    /// precedent): WorkflowPaused/WorkflowResumed interleaved mid-history are
    /// never matched, mismatched, or consumed by the cursor — an activity
    /// recorded around them resolves exactly as if they were absent.
    #[test]
    fn pause_resume_markers_are_invisible_to_the_cursor() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut cursor = HistoryCursor::new(vec![
            workflow_started(1)?,
            paused(2)?,
            scheduled(3, 0)?,
            resumed(4)?,
            completed(5, 0)?,
        ])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));
        match result {
            CursorResolveResult::Matched(events) => {
                assert_eq!(
                    events.len(),
                    2,
                    "only the activity's own events are consumed"
                );
                assert!(matches!(
                    events.last(),
                    Some(Event::ActivityCompleted { .. })
                ));
            }
            other => {
                return Err(
                    format!("pause/resume markers must not disturb replay, got {other:?}").into(),
                );
            }
        }

        // The markers are never matched: a further resolve for any activity
        // exhausts (the leftover WorkflowResumed marker is invisible, never a
        // spurious match or mismatch).
        assert_eq!(
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(1)),
            CursorResolveResult::Exhausted,
            "the leftover pause/resume markers are invisible, never matched"
        );
        Ok(())
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

    fn child_id(value: u128) -> WorkflowId {
        WorkflowId::new(Uuid::from_u128(value))
    }

    fn child_started(seq: u64, child: u128) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ChildWorkflowStarted {
            envelope: envelope(seq)?,
            child_workflow_id: child_id(child),
            workflow_type: "child".to_owned(),
            input: payload()?,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn child_completed(seq: u64, child: u128) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ChildWorkflowCompleted {
            envelope: envelope(seq)?,
            child_workflow_id: child_id(child),
            result: payload()?,
        })
    }

    fn signal_received(seq: u64, name: &str) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::SignalReceived {
            envelope: envelope(seq)?,
            name: name.to_owned(),
            payload: payload()?,
        })
    }

    #[test]
    fn fast_forward_to_child_terminal_skips_consumed_commands()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            completed(2, 0)?,
            child_started(3, 1)?,
            signal_received(4, "mid")?,
            child_started(5, 2)?,
            child_completed(6, 1)?,
        ])?;

        cursor.fast_forward_to_child_terminal(&child_id(1));
        let result = cursor.resolve_child_terminal(&child_id(1));

        match result {
            ChildTerminalResolveResult::Matched(events) => {
                assert_eq!(events.len(), 1);
                assert!(matches!(
                    events.first(),
                    Some(Event::ChildWorkflowCompleted { child_workflow_id, .. })
                        if *child_workflow_id == child_id(1)
                ));
            }
            ChildTerminalResolveResult::Exhausted | ChildTerminalResolveResult::Mismatch { .. } => {
                return Err("await must reach the awaited child's recorded terminal".into());
            }
        }
        Ok(())
    }

    #[test]
    fn fast_forward_to_child_terminal_exhausts_when_no_terminal_recorded()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            completed(2, 0)?,
            child_started(3, 1)?,
        ])?;

        cursor.fast_forward_to_child_terminal(&child_id(1));

        assert_eq!(
            cursor.resolve_child_terminal(&child_id(1)),
            ChildTerminalResolveResult::Exhausted
        );
        Ok(())
    }

    #[test]
    fn resolve_child_terminal_reports_mismatch_without_skipping_in_strict_replay()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![scheduled(1, 0)?, child_completed(2, 1)?])?;

        let result = cursor.resolve_child_terminal(&child_id(1));

        match result {
            ChildTerminalResolveResult::Mismatch { found } => {
                assert_eq!(found.seq, 1);
                assert_eq!(found.family, Some(RecordedEventFamily::Activity));
            }
            ChildTerminalResolveResult::Matched(_) | ChildTerminalResolveResult::Exhausted => {
                return Err("strict replay must not skip an unconsumed recorded command".into());
            }
        }
        Ok(())
    }

    fn child_failed(seq: u64, child: u128) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ChildWorkflowFailed {
            envelope: envelope(seq)?,
            child_workflow_id: child_id(child),
            error: aion_core::WorkflowError {
                message: "child failed".to_owned(),
                details: None,
            },
        })
    }

    #[test]
    fn resolve_activity_skips_interleaved_signal_and_leaves_it_matchable()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            signal_received(2, "mid")?,
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
                assert!(matches!(
                    events.last(),
                    Some(Event::ActivityCompleted { activity_id, .. })
                        if activity_id.sequence_position() == 0
                ));
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err(
                    "an async signal arrival inside the activity range must not fail replay".into(),
                );
            }
        }

        let signal = cursor.resolve_next(
            RecordedEventFamily::Signal,
            CorrelationKey::Signal {
                name: "mid".to_owned(),
                index: 0,
            },
        );
        match signal {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 1);
                assert!(matches!(events.first(), Some(Event::SignalReceived { .. })));
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("the skipped signal must stay matchable for its own command".into());
            }
        }
        Ok(())
    }

    #[test]
    fn resolve_activity_resolves_interleaved_parallel_activity_ranges()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![
            scheduled(1, 0)?,
            scheduled(2, 1)?,
            completed(3, 1)?,
            completed(4, 0)?,
        ])?;

        let first = cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));
        match first {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 2);
                assert!(matches!(
                    events.last(),
                    Some(Event::ActivityCompleted { activity_id, .. })
                        if activity_id.sequence_position() == 0
                ));
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("a parallel activity's events inside the range must be skipped".into());
            }
        }

        let second =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(1));
        match second {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 2);
                assert!(matches!(
                    events.last(),
                    Some(Event::ActivityCompleted { activity_id, .. })
                        if activity_id.sequence_position() == 1
                ));
                assert_eq!(cursor.current_sequence(), None);
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("the interleaved activity must remain resolvable afterwards".into());
            }
        }
        Ok(())
    }

    #[test]
    fn resolve_activity_skips_interleaved_child_terminal() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut cursor = HistoryCursor::new(vec![
            child_started(1, 7)?,
            scheduled(2, 0)?,
            child_completed(3, 7)?,
            child_failed(4, 9)?,
            completed(5, 0)?,
        ])?;

        cursor.fast_forward_to_key(&CorrelationKey::Activity(0));
        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Matched(events) => {
                assert_eq!(events.len(), 2);
                assert!(matches!(
                    events.last(),
                    Some(Event::ActivityCompleted { activity_id, .. })
                        if activity_id.sequence_position() == 0
                ));
            }
            CursorResolveResult::Exhausted | CursorResolveResult::Mismatch { .. } => {
                return Err("child terminals inside the activity range must be skipped".into());
            }
        }
        Ok(())
    }

    #[test]
    fn resolve_activity_still_mismatches_on_wrong_anchor_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = HistoryCursor::new(vec![scheduled(1, 1)?, completed(2, 1)?])?;

        let result =
            cursor.resolve_next(RecordedEventFamily::Activity, CorrelationKey::Activity(0));

        match result {
            CursorResolveResult::Mismatch {
                expected_key,
                found,
            } => {
                assert_eq!(expected_key, CorrelationKey::Activity(0));
                assert_eq!(found.key, Some(CorrelationKey::Activity(1)));
            }
            CursorResolveResult::Matched(_) | CursorResolveResult::Exhausted => {
                return Err("a wrong key at the Scheduled anchor must stay a mismatch".into());
            }
        }
        Ok(())
    }

    #[test]
    fn fast_forward_and_resolution_smoke_over_large_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let count: u64 = 5_000;
        let mut events = Vec::with_capacity(usize::try_from(count * 2)?);
        for ordinal in 0..count {
            events.push(scheduled(ordinal * 2 + 1, ordinal)?);
            events.push(completed(ordinal * 2 + 2, ordinal)?);
        }
        let mut cursor = HistoryCursor::new(events)?;

        for ordinal in 0..count {
            let key = CorrelationKey::Activity(ordinal);
            cursor.fast_forward_to_key(&key);
            let result = cursor.resolve_next(RecordedEventFamily::Activity, key);
            assert!(
                matches!(result, CursorResolveResult::Matched(ref events) if events.len() == 2),
                "ordinal {ordinal} failed to resolve in the large-history smoke"
            );
        }
        assert_eq!(cursor.current_sequence(), None);
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
