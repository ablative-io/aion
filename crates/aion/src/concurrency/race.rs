//! race: first wins, cancel + record the rest

use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
use chrono::{DateTime, Utc};

use crate::concurrency::{
    CancellationRecordingContext, CorrelatedOutcome, CorrelatedResult, CorrelatedResultTable,
    CorrelatedSlotState, CorrelationBatch, CorrelationError, CorrelationMailbox, CorrelationToken,
    InFlightChild, LinkedChild, cancel_remaining,
};
use crate::engine_seam::{ChildWorkflowSpawnRequest, EngineHandle, EngineSeamError};

/// Metadata used to envelope race child observations before routing through the recorder seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaceRecordingContext {
    parent_workflow_id: WorkflowId,
    next_seq: u64,
    recorded_at: DateTime<Utc>,
}

impl RaceRecordingContext {
    /// Creates a recording context with caller-controlled sequence and time.
    #[must_use]
    pub const fn new(
        parent_workflow_id: WorkflowId,
        next_seq: u64,
        recorded_at: DateTime<Utc>,
    ) -> Self {
        Self {
            parent_workflow_id,
            next_seq,
            recorded_at,
        }
    }

    /// Returns the next workflow-history sequence position used as the race token base.
    #[must_use]
    pub const fn next_sequence_position(&self) -> u64 {
        self.next_seq
    }

    fn parent_workflow_id(&self) -> &WorkflowId {
        &self.parent_workflow_id
    }

    fn next_envelope(&mut self) -> Result<EventEnvelope, RaceError> {
        let seq = self.next_seq;
        let envelope = EventEnvelope {
            seq,
            recorded_at: self.recorded_at,
            workflow_id: self.parent_workflow_id.clone(),
        };
        self.next_seq = seq
            .checked_add(1)
            .ok_or(RaceError::SequenceOverflow { seq })?;
        Ok(envelope)
    }

    fn cancellation_recording(&self) -> CancellationRecordingContext {
        CancellationRecordingContext::new(
            self.parent_workflow_id.clone(),
            self.next_seq,
            self.recorded_at,
        )
    }

    fn advance_after_cancellations(&mut self, cancellations: usize) -> Result<(), RaceError> {
        let increment = u64::try_from(cancellations)
            .map_err(|_| RaceError::SequenceOverflow { seq: self.next_seq })?;
        self.next_seq = self
            .next_seq
            .checked_add(increment)
            .ok_or(RaceError::SequenceOverflow { seq: self.next_seq })?;
        Ok(())
    }
}

/// Child workflow specification spawned by [`race`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaceChildSpec {
    /// Child workflow type selected by the parent workflow.
    pub workflow_type: String,
    /// Opaque child workflow input payload.
    pub input: Payload,
    /// Pre-allocated child workflow identifier recorded before the start.
    pub child_workflow_id: WorkflowId,
}

impl RaceChildSpec {
    /// Creates a child workflow spec for a race fan-out.
    #[must_use]
    pub fn new(
        workflow_type: impl Into<String>,
        input: Payload,
        child_workflow_id: WorkflowId,
    ) -> Self {
        Self {
            workflow_type: workflow_type.into(),
            input,
            child_workflow_id,
        }
    }
}

/// First child outcome selected by [`race`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaceWinner {
    /// Original fan-out input position that settled first.
    pub index: usize,
    /// Correlation token echoed by the winning child result message.
    pub token: CorrelationToken,
    /// First terminal child outcome observed by the parent mailbox.
    pub outcome: CorrelatedOutcome,
}

impl From<CorrelatedResult> for RaceWinner {
    fn from(result: CorrelatedResult) -> Self {
        Self {
            index: result.index,
            token: result.token,
            outcome: result.outcome,
        }
    }
}

/// Errors produced by race fan-out collection.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum RaceError {
    /// A race requires at least one child spec.
    #[error("race requires at least one child spec")]
    Empty,
    /// Workflow-history event sequencing would overflow.
    #[error("race event sequence overflow at {seq}")]
    SequenceOverflow {
        /// Sequence value that could not be advanced.
        seq: u64,
    },
    /// AE or AD rejected a seam operation.
    #[error(transparent)]
    Engine(#[from] EngineSeamError),
    /// Correlation derivation, matching, or loser cancellation failed.
    #[error(transparent)]
    Correlation(#[from] CorrelationError),
    /// AE started a child under a different identity than the recorded one.
    #[error(
        "child workflow spawn returned invalid child id {child_workflow_id} for parent {parent_workflow_id}"
    )]
    InvalidChildIdentity {
        /// Parent workflow that requested the child.
        parent_workflow_id: WorkflowId,
        /// Child workflow identity returned by AE.
        child_workflow_id: WorkflowId,
    },
}

/// Spawns all child specs as linked children, returns the first terminal outcome, and cancels losers.
///
/// `race` resolves on first settle: the first completion, failure, or cancellation message wins. If
/// the first child result to arrive is a failure, this function records and returns that failure
/// outcome rather than waiting for a successful child.
///
/// # Errors
///
/// Returns [`RaceError`] if spawning, recording, mailbox selection, or loser cancellation fails.
pub fn race(
    engine: &impl EngineHandle,
    recording: &mut RaceRecordingContext,
    mailbox: &mut impl CorrelationMailbox,
    specs: &[RaceChildSpec],
) -> Result<RaceWinner, RaceError> {
    if specs.is_empty() {
        return Err(RaceError::Empty);
    }

    let batch = CorrelationBatch::from_base(recording.next_sequence_position(), specs.len())?;
    let mut table = CorrelatedResultTable::new(batch.clone());
    let children = spawn_linked_children(engine, recording, &batch, specs)?;

    let selected = match mailbox.receive_correlated(&table) {
        Ok(selected) => selected,
        Err(error) => {
            cancel_spawned_children(engine, recording, &batch, &children)?;
            return Err(RaceError::Correlation(error));
        }
    };
    record_winner(engine, recording, &selected.outcome)?;
    let winner = RaceWinner::from(selected.clone());
    table.apply_result(selected);

    let loser_count = pending_count(&table);
    let mut cancellation_recording = recording.cancellation_recording();
    cancel_remaining(engine, &mut cancellation_recording, &mut table, &children)?;
    recording.advance_after_cancellations(loser_count)?;

    Ok(winner)
}

fn spawn_linked_children(
    engine: &impl EngineHandle,
    recording: &mut RaceRecordingContext,
    batch: &CorrelationBatch,
    specs: &[RaceChildSpec],
) -> Result<Vec<InFlightChild>, RaceError> {
    let mut children = Vec::with_capacity(specs.len());
    for (slot, spec) in batch.slots().iter().copied().zip(specs.iter()) {
        // Record-then-spawn (#56): the pre-allocated id is durably recorded
        // before AE is asked to start the child.
        let event = Event::ChildWorkflowStarted {
            envelope: recording.next_envelope()?,
            child_workflow_id: spec.child_workflow_id.clone(),
            workflow_type: spec.workflow_type.clone(),
            input: spec.input.clone(),
        };
        if let Err(error) = engine.record_workflow_event(recording.parent_workflow_id(), event) {
            cancel_spawned_children(engine, recording, batch, &children)?;
            return Err(RaceError::Engine(error));
        }
        let request = ChildWorkflowSpawnRequest {
            parent_workflow_id: recording.parent_workflow_id().clone(),
            child_workflow_id: spec.child_workflow_id.clone(),
            workflow_type: spec.workflow_type.clone(),
            input: spec.input.clone(),
        };
        let spawned = match engine.spawn_child_workflow(request) {
            Ok(spawned) => spawned,
            Err(error) => {
                // The recorded start survives the failed request by design
                // (the crash-window record the recovery sweep repairs from).
                cancel_spawned_children(engine, recording, batch, &children)?;
                return Err(RaceError::Engine(error));
            }
        };
        if spawned.child_workflow_id != spec.child_workflow_id {
            engine.terminate_linked_child_workflow(
                recording.parent_workflow_id(),
                spawned.child_process,
                slot.token().value(),
            )?;
            cancel_spawned_children(engine, recording, batch, &children)?;
            return Err(RaceError::InvalidChildIdentity {
                parent_workflow_id: recording.parent_workflow_id().clone(),
                child_workflow_id: spawned.child_workflow_id,
            });
        }
        children.push(InFlightChild::new(
            slot.index(),
            slot.token(),
            LinkedChild::Workflow {
                workflow_id: spawned.child_workflow_id,
                process: spawned.child_process,
            },
        ));
    }
    Ok(children)
}

fn cancel_spawned_children(
    engine: &impl EngineHandle,
    recording: &mut RaceRecordingContext,
    batch: &CorrelationBatch,
    children: &[InFlightChild],
) -> Result<(), RaceError> {
    if children.is_empty() {
        return Ok(());
    }

    let mut table = CorrelatedResultTable::new(batch.clone());
    let loser_count = children.len();
    let mut cancellation_recording = recording.cancellation_recording();
    cancel_remaining(engine, &mut cancellation_recording, &mut table, children)?;
    recording.advance_after_cancellations(loser_count)?;
    Ok(())
}

fn record_winner(
    engine: &impl EngineHandle,
    recording: &mut RaceRecordingContext,
    outcome: &CorrelatedOutcome,
) -> Result<(), RaceError> {
    let event = match outcome {
        CorrelatedOutcome::ChildWorkflowCompleted {
            child_workflow_id,
            result,
        } => Event::ChildWorkflowCompleted {
            envelope: recording.next_envelope()?,
            child_workflow_id: child_workflow_id.clone(),
            result: result.clone(),
        },
        CorrelatedOutcome::ChildWorkflowFailed {
            child_workflow_id,
            error,
        } => Event::ChildWorkflowFailed {
            envelope: recording.next_envelope()?,
            child_workflow_id: child_workflow_id.clone(),
            error: error.clone(),
        },
        CorrelatedOutcome::ChildWorkflowCancelled { child_workflow_id } => {
            Event::ChildWorkflowCancelled {
                envelope: recording.next_envelope()?,
                child_workflow_id: child_workflow_id.clone(),
            }
        }
    };
    engine.record_workflow_event(recording.parent_workflow_id(), event)?;
    Ok(())
}

fn pending_count(table: &CorrelatedResultTable) -> usize {
    table
        .states()
        .iter()
        .filter(|state| matches!(state, CorrelatedSlotState::Pending))
        .count()
}

#[cfg(test)]
mod tests {
    use aion_core::{ContentType, Event, Payload, WorkflowError, WorkflowId};
    use chrono::DateTime;

    use super::{RaceChildSpec, RaceError, RaceRecordingContext, RaceWinner, race};
    use crate::concurrency::{
        CorrelatedOutcome, CorrelationBatch, CorrelationError, VecCorrelationMailbox,
    };
    use crate::engine_seam::test_support::FakeEngineHandle;
    use crate::engine_seam::{
        ChildWorkflowSpawnResult, WorkflowMailboxMessage, WorkflowProcessHandle,
    };

    #[test]
    fn returns_second_spawn_when_its_result_arrives_first() -> Result<(), Box<dyn std::error::Error>>
    {
        let parent = WorkflowId::new_v4();
        let children = [
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        let mut mailbox =
            VecCorrelationMailbox::new(vec![completed(children[1].clone(), 101, b"second")]);
        let engine = engine_with_children(&children)?;
        let specs = specs(&children);
        let mut recording = recording(parent.clone(), 100)?;

        let winner = race(&engine, &mut recording, &mut mailbox, &specs)?;

        assert_eq!(winner.index, 1);
        assert_eq!(winner.token.value(), 101);
        assert!(matches!(
            winner.outcome,
            CorrelatedOutcome::ChildWorkflowCompleted { child_workflow_id, result }
                if child_workflow_id == children[1] && result == payload(b"second")
        ));
        Ok(())
    }

    #[test]
    fn first_arriving_failure_wins() -> Result<(), Box<dyn std::error::Error>> {
        let parent = WorkflowId::new_v4();
        let children = [
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        let error = WorkflowError {
            message: "child failed first".to_owned(),
            details: None,
        };
        let mut mailbox =
            VecCorrelationMailbox::new(vec![failed(children[0].clone(), 200, error.clone())]);
        let engine = engine_with_children(&children)?;
        let specs = specs(&children);
        let mut recording = recording(parent.clone(), 200)?;

        let winner = race(&engine, &mut recording, &mut mailbox, &specs)?;

        assert_eq!(winner.index, 0);
        assert!(matches!(
            winner.outcome,
            CorrelatedOutcome::ChildWorkflowFailed { child_workflow_id, error: observed }
                if child_workflow_id == children[0] && observed == error
        ));
        let events = engine.recorded_events()?;
        assert!(matches!(
            &events[3],
            (workflow_id, Event::ChildWorkflowFailed { envelope, child_workflow_id, error: observed })
                if workflow_id == &parent
                    && envelope.seq == 203
                    && child_workflow_id == &children[0]
                    && observed == &error
        ));
        Ok(())
    }

    #[test]
    fn cancels_losers_and_late_loser_result_does_not_change_outcome()
    -> Result<(), Box<dyn std::error::Error>> {
        let parent = WorkflowId::new_v4();
        let children = [
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        let processes = [
            WorkflowProcessHandle::new(11),
            WorkflowProcessHandle::new(12),
            WorkflowProcessHandle::new(13),
        ];
        let mut mailbox = VecCorrelationMailbox::new(vec![
            completed(children[1].clone(), 301, b"winner"),
            completed(children[0].clone(), 300, b"late"),
        ]);
        let engine = FakeEngineHandle::new();
        for (child, process) in children.iter().cloned().zip(processes) {
            engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
                child_workflow_id: child,
                child_process: process,
            }))?;
        }
        let specs = specs(&children);
        let mut recording = recording(parent.clone(), 300)?;

        let winner = race(&engine, &mut recording, &mut mailbox, &specs)?;

        assert_eq!(
            winner,
            RaceWinner {
                index: 1,
                token: CorrelationBatch::from_base(300, 3)?.slots()[1].token(),
                outcome: CorrelatedOutcome::ChildWorkflowCompleted {
                    child_workflow_id: children[1].clone(),
                    result: payload(b"winner"),
                },
            }
        );
        assert_eq!(
            engine.terminated_child_workflows()?,
            vec![
                (parent.clone(), processes[0], 300),
                (parent.clone(), processes[2], 302)
            ]
        );
        let events = engine.recorded_events()?;
        assert_eq!(events.len(), 6);
        assert!(matches!(
            &events[4],
            (workflow_id, Event::ChildWorkflowCancelled { envelope, child_workflow_id })
                if workflow_id == &parent && envelope.seq == 304 && child_workflow_id == &children[0]
        ));
        assert!(matches!(
            &events[5],
            (workflow_id, Event::ChildWorkflowCancelled { envelope, child_workflow_id })
                if workflow_id == &parent && envelope.seq == 305 && child_workflow_id == &children[2]
        ));

        assert_eq!(engine.recorded_events()?.len(), 6);
        assert_eq!(mailbox.len(), 1);
        Ok(())
    }

    #[test]
    fn cancels_all_children_when_mailbox_closes_before_a_winner()
    -> Result<(), Box<dyn std::error::Error>> {
        let parent = WorkflowId::new_v4();
        let children = [
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        let processes = [
            WorkflowProcessHandle::new(21),
            WorkflowProcessHandle::new(22),
            WorkflowProcessHandle::new(23),
        ];
        let mut mailbox = VecCorrelationMailbox::new(Vec::new());
        let engine = FakeEngineHandle::new();
        for (child, process) in children.iter().cloned().zip(processes) {
            engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
                child_workflow_id: child,
                child_process: process,
            }))?;
        }
        let specs = specs(&children);
        let mut recording = recording(parent.clone(), 400)?;

        let error = match race(&engine, &mut recording, &mut mailbox, &specs) {
            Ok(winner) => {
                return Err(
                    format!("empty mailbox unexpectedly returned winner {winner:?}").into(),
                );
            }
            Err(error) => error,
        };

        assert!(matches!(
            error,
            RaceError::Correlation(CorrelationError::MailboxClosed)
        ));
        assert_eq!(
            engine.terminated_child_workflows()?,
            vec![
                (parent.clone(), processes[0], 400),
                (parent.clone(), processes[1], 401),
                (parent.clone(), processes[2], 402)
            ]
        );
        let events = engine.recorded_events()?;
        assert_eq!(events.len(), 6);
        assert!(
            events[3..]
                .iter()
                .all(|(workflow_id, event)| workflow_id == &parent
                    && matches!(event, Event::ChildWorkflowCancelled { .. }))
        );
        Ok(())
    }

    fn engine_with_children(
        children: &[WorkflowId; 3],
    ) -> Result<FakeEngineHandle, Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        for (index, child) in children.iter().cloned().enumerate() {
            let pid = u64::try_from(index + 1)?;
            engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
                child_workflow_id: child,
                child_process: WorkflowProcessHandle::new(pid),
            }))?;
        }
        Ok(engine)
    }

    fn specs(children: &[WorkflowId; 3]) -> Vec<RaceChildSpec> {
        children
            .iter()
            .enumerate()
            .map(|(index, child)| {
                RaceChildSpec::new(format!("child.{index}"), payload(b"{}"), child.clone())
            })
            .collect()
    }

    fn recording(
        parent: WorkflowId,
        next_seq: u64,
    ) -> Result<RaceRecordingContext, Box<dyn std::error::Error>> {
        let recorded_at =
            DateTime::parse_from_rfc3339("2026-06-04T12:00:00Z").map(DateTime::from)?;
        Ok(RaceRecordingContext::new(parent, next_seq, recorded_at))
    }

    fn completed(
        child_workflow_id: WorkflowId,
        correlation: u64,
        bytes: &'static [u8],
    ) -> WorkflowMailboxMessage {
        WorkflowMailboxMessage::ChildWorkflowCompleted {
            child_workflow_id,
            correlation,
            result: payload(bytes),
        }
    }

    fn failed(
        child_workflow_id: WorkflowId,
        correlation: u64,
        error: WorkflowError,
    ) -> WorkflowMailboxMessage {
        WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation,
            error,
        }
    }

    fn payload(bytes: &'static [u8]) -> Payload {
        Payload::new(ContentType::Json, bytes.to_vec())
    }
}
