//! Per-spawn correlation tokens, selective-receive matching, and cancellation helpers.
//!
//! The collectors built on this module must run in a workflow process that traps exits before
//! cancelling linked children. With trapped exits, BEAM converts each killed child exit into a
//! mailbox message the collector can drain instead of letting the exit signal terminate the parent.

use std::collections::{HashMap, VecDeque};

use aion_core::{ActivityId, Event, EventEnvelope, Payload, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

use crate::Pid;
#[cfg(test)]
use crate::engine_seam::test_support::DeliveredWorkflowMessage;
use crate::engine_seam::{
    EngineHandle, EngineSeamError, WorkflowMailboxMessage, WorkflowProcessHandle,
};

/// Deterministic per-spawn token carried by child result mailbox messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CorrelationToken(u64);

impl CorrelationToken {
    /// Derives a token from the workflow's spawning sequence position.
    #[must_use]
    pub const fn from_sequence_position(sequence_position: u64) -> Self {
        Self(sequence_position)
    }

    /// Returns the sequence position used to derive this token.
    #[must_use]
    pub const fn sequence_position(self) -> u64 {
        self.0
    }

    /// Returns the raw value used by current mailbox messages.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl From<CorrelationToken> for u64 {
    fn from(token: CorrelationToken) -> Self {
        token.value()
    }
}

/// Errors produced while deriving, matching, or cancelling correlated children.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum CorrelationError {
    /// Adding the fan-out index to the base sequence would overflow.
    #[error(
        "correlation token overflow for base sequence {base_sequence_position} at index {index}"
    )]
    TokenOverflow {
        /// First sequence position in the fan-out batch.
        base_sequence_position: u64,
        /// Spawn index that overflowed.
        index: usize,
    },

    /// The same token appeared more than once in a batch.
    #[error("duplicate correlation token {token} in fan-out batch")]
    DuplicateToken {
        /// Duplicated raw token value.
        token: u64,
    },

    /// No pending correlated child outcome is currently available.
    #[error("mailbox closed before a pending correlated child outcome arrived")]
    MailboxClosed,

    /// Cancellation event sequencing would overflow the parent workflow history.
    #[error("cancellation event sequence overflow at {seq}")]
    SequenceOverflow {
        /// Sequence value that could not be advanced.
        seq: u64,
    },

    /// AE or AD rejected a seam operation.
    #[error(transparent)]
    Engine(#[from] EngineSeamError),
}

/// Derives one unique token per fan-out spawn position.
///
/// # Errors
///
/// Returns [`CorrelationError::TokenOverflow`] if `base_sequence_position + index` would wrap.
pub fn derive_batch(
    base_sequence_position: u64,
    len: usize,
) -> Result<Vec<CorrelationToken>, CorrelationError> {
    (0..len)
        .map(|index| {
            let offset = u64::try_from(index).map_err(|_| CorrelationError::TokenOverflow {
                base_sequence_position,
                index,
            })?;
            base_sequence_position
                .checked_add(offset)
                .map(CorrelationToken::from_sequence_position)
                .ok_or(CorrelationError::TokenOverflow {
                    base_sequence_position,
                    index,
                })
        })
        .collect()
}

/// A spawn position and its deterministic token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnSlot {
    index: usize,
    token: CorrelationToken,
}

impl SpawnSlot {
    /// Creates a spawn slot from an input index and token.
    #[must_use]
    pub const fn new(index: usize, token: CorrelationToken) -> Self {
        Self { index, token }
    }

    /// Original fan-out input position.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    /// Token carried by the spawned child and echoed in its result message.
    #[must_use]
    pub const fn token(self) -> CorrelationToken {
        self.token
    }
}

/// Batch metadata used to route result messages back to spawn positions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrelationBatch {
    slots: Vec<SpawnSlot>,
    by_token: HashMap<CorrelationToken, usize>,
}

impl CorrelationBatch {
    /// Builds batch metadata from a deterministic base sequence and fan-out length.
    ///
    /// # Errors
    ///
    /// Returns [`CorrelationError`] if token derivation overflows or duplicates are found.
    pub fn from_base(base_sequence_position: u64, len: usize) -> Result<Self, CorrelationError> {
        let tokens = derive_batch(base_sequence_position, len)?;
        Self::from_tokens(tokens)
    }

    /// Builds batch metadata from already-derived tokens.
    ///
    /// # Errors
    ///
    /// Returns [`CorrelationError::DuplicateToken`] if tokens are not unique.
    pub fn from_tokens(tokens: Vec<CorrelationToken>) -> Result<Self, CorrelationError> {
        let mut by_token = HashMap::with_capacity(tokens.len());
        let mut slots = Vec::with_capacity(tokens.len());
        for (index, token) in tokens.into_iter().enumerate() {
            if by_token.insert(token, index).is_some() {
                return Err(CorrelationError::DuplicateToken {
                    token: token.value(),
                });
            }
            slots.push(SpawnSlot::new(index, token));
        }
        Ok(Self { slots, by_token })
    }

    /// Number of spawn slots in the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Returns true when the batch has no slots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Returns all slots in input order.
    #[must_use]
    pub fn slots(&self) -> &[SpawnSlot] {
        &self.slots
    }

    /// Finds the original spawn index for a token.
    #[must_use]
    pub fn index_for(&self, token: CorrelationToken) -> Option<usize> {
        self.by_token.get(&token).copied()
    }
}

/// Child outcome selected from the parent workflow mailbox by correlation token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorrelatedOutcome {
    /// Child workflow completed successfully.
    ChildWorkflowCompleted {
        /// Child workflow that produced the result.
        child_workflow_id: WorkflowId,
        /// Opaque child result payload.
        result: Payload,
    },
    /// Child workflow failed terminally.
    ChildWorkflowFailed {
        /// Child workflow that failed.
        child_workflow_id: WorkflowId,
        /// Terminal child failure.
        error: WorkflowError,
    },
    /// Child workflow was cancelled.
    ChildWorkflowCancelled {
        /// Child workflow that was cancelled.
        child_workflow_id: WorkflowId,
    },
}

/// A correlated outcome routed to its original spawn position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrelatedResult {
    /// Original fan-out input position.
    pub index: usize,
    /// Token echoed by the child result message.
    pub token: CorrelationToken,
    /// Outcome carried by the child result message.
    pub outcome: CorrelatedOutcome,
}

/// Slot state maintained by collectors so cancellation cannot be overwritten by late results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorrelatedSlotState {
    /// The child is still in flight.
    Pending,
    /// The child produced a terminal outcome.
    Settled(CorrelatedOutcome),
    /// The child was cancelled and a concrete cancellation event was recorded.
    Cancelled,
}

/// Mutable table of correlated child results in original spawn order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrelatedResultTable {
    batch: CorrelationBatch,
    states: Vec<CorrelatedSlotState>,
}

impl CorrelatedResultTable {
    /// Creates a pending result table for the supplied batch.
    #[must_use]
    pub fn new(batch: CorrelationBatch) -> Self {
        let states = vec![CorrelatedSlotState::Pending; batch.len()];
        Self { batch, states }
    }

    /// Returns batch metadata backing this table.
    #[must_use]
    pub const fn batch(&self) -> &CorrelationBatch {
        &self.batch
    }

    /// Returns slot states in input order.
    #[must_use]
    pub fn states(&self) -> &[CorrelatedSlotState] {
        &self.states
    }

    /// Records a mailbox outcome unless the slot is already settled or cancelled.
    ///
    /// Returns true when the outcome changed a pending slot. Late duplicate results return false.
    pub fn apply_result(&mut self, result: CorrelatedResult) -> bool {
        let Some(state) = self.states.get_mut(result.index) else {
            return false;
        };
        if matches!(state, CorrelatedSlotState::Pending) {
            *state = CorrelatedSlotState::Settled(result.outcome);
            true
        } else {
            false
        }
    }

    fn mark_cancelled(&mut self, index: usize) -> bool {
        let Some(state) = self.states.get_mut(index) else {
            return false;
        };
        if matches!(state, CorrelatedSlotState::Pending) {
            *state = CorrelatedSlotState::Cancelled;
            true
        } else {
            false
        }
    }

    fn is_pending(&self, index: usize) -> bool {
        self.states
            .get(index)
            .is_some_and(|state| matches!(state, CorrelatedSlotState::Pending))
    }
}

/// Parent mailbox abstraction used by correlated selective receive.
pub trait CorrelationMailbox {
    /// Receives the next child outcome whose correlation token belongs to `table` and is pending.
    ///
    /// Implementations must preserve unrelated messages and late results for already-settled slots.
    ///
    /// # Errors
    ///
    /// Returns [`CorrelationError`] if no matching pending child outcome can be selected.
    fn receive_correlated(
        &mut self,
        table: &CorrelatedResultTable,
    ) -> Result<CorrelatedResult, CorrelationError>;
}

/// Simple FIFO mailbox useful for tests and embedding contexts that already drained messages.
#[derive(Debug, Default)]
pub struct VecCorrelationMailbox {
    messages: VecDeque<WorkflowMailboxMessage>,
}

impl VecCorrelationMailbox {
    /// Creates a FIFO mailbox from an ordered message list.
    #[must_use]
    pub fn new(messages: Vec<WorkflowMailboxMessage>) -> Self {
        Self {
            messages: VecDeque::from(messages),
        }
    }

    /// Returns the number of messages still queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Returns true when no messages remain queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Returns a projection of queued messages suitable for equality assertions in tests.
    #[cfg(test)]
    #[must_use]
    pub fn delivered_messages(&self) -> Vec<DeliveredWorkflowMessage> {
        self.messages
            .iter()
            .map(DeliveredWorkflowMessage::from_message)
            .collect()
    }
}

impl CorrelationMailbox for VecCorrelationMailbox {
    fn receive_correlated(
        &mut self,
        table: &CorrelatedResultTable,
    ) -> Result<CorrelatedResult, CorrelationError> {
        let mut preserved = VecDeque::with_capacity(self.messages.len());
        let mut selected = None;

        while let Some(message) = self.messages.pop_front() {
            match correlated_result_from_message(message, table) {
                Ok(result) => {
                    selected = Some(result);
                    break;
                }
                Err(message) => preserved.push_back(message),
            }
        }

        preserved.append(&mut self.messages);
        self.messages = preserved;

        selected.ok_or(CorrelationError::MailboxClosed)
    }
}

fn correlated_result_from_message(
    message: WorkflowMailboxMessage,
    table: &CorrelatedResultTable,
) -> Result<CorrelatedResult, WorkflowMailboxMessage> {
    match message {
        WorkflowMailboxMessage::ChildWorkflowCompleted {
            child_workflow_id,
            correlation,
            result,
        } => {
            let token = CorrelationToken::from_sequence_position(correlation);
            let Some(index) = pending_index_for(table, token) else {
                return Err(WorkflowMailboxMessage::ChildWorkflowCompleted {
                    child_workflow_id,
                    correlation,
                    result,
                });
            };
            Ok(CorrelatedResult {
                index,
                token,
                outcome: CorrelatedOutcome::ChildWorkflowCompleted {
                    child_workflow_id,
                    result,
                },
            })
        }
        WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation,
            error,
        } => {
            let token = CorrelationToken::from_sequence_position(correlation);
            let Some(index) = pending_index_for(table, token) else {
                return Err(WorkflowMailboxMessage::ChildWorkflowFailed {
                    child_workflow_id,
                    correlation,
                    error,
                });
            };
            Ok(CorrelatedResult {
                index,
                token,
                outcome: CorrelatedOutcome::ChildWorkflowFailed {
                    child_workflow_id,
                    error,
                },
            })
        }
        WorkflowMailboxMessage::ChildWorkflowCancelled {
            child_workflow_id,
            correlation,
        } => {
            let token = CorrelationToken::from_sequence_position(correlation);
            let Some(index) = pending_index_for(table, token) else {
                return Err(WorkflowMailboxMessage::ChildWorkflowCancelled {
                    child_workflow_id,
                    correlation,
                });
            };
            Ok(CorrelatedResult {
                index,
                token,
                outcome: CorrelatedOutcome::ChildWorkflowCancelled { child_workflow_id },
            })
        }
        other => Err(other),
    }
}

fn pending_index_for(table: &CorrelatedResultTable, token: CorrelationToken) -> Option<usize> {
    table
        .batch
        .index_for(token)
        .filter(|index| table.is_pending(*index))
}

/// Linked child metadata used by cancellation helpers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InFlightChild {
    /// Original fan-out input position.
    pub index: usize,
    /// Token assigned to this spawn.
    pub token: CorrelationToken,
    /// Concrete linked child to terminate and record if still pending.
    pub child: LinkedChild,
}

impl InFlightChild {
    /// Creates metadata for an in-flight child.
    #[must_use]
    pub const fn new(index: usize, token: CorrelationToken, child: LinkedChild) -> Self {
        Self {
            index,
            token,
            child,
        }
    }
}

/// Concrete linked child kinds supported by cancellation propagation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkedChild {
    /// In-VM activity child linked under this workflow process.
    Activity {
        /// Activity identifier to record as cancelled.
        activity_id: ActivityId,
        /// Live BEAM process id to terminate through AE.
        process: Pid,
    },
    /// Linked child workflow execution with its own history.
    Workflow {
        /// Child workflow identifier to record as cancelled.
        workflow_id: WorkflowId,
        /// Live workflow process handle to terminate through AE.
        process: WorkflowProcessHandle,
    },
}

/// Metadata used to envelope cancellation events before routing through the recorder seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancellationRecordingContext {
    parent_workflow_id: WorkflowId,
    next_seq: u64,
    recorded_at: DateTime<Utc>,
}

impl CancellationRecordingContext {
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

    fn next_envelope(&mut self) -> Result<EventEnvelope, CorrelationError> {
        let seq = self.next_seq;
        let envelope = EventEnvelope {
            seq,
            recorded_at: self.recorded_at,
            workflow_id: self.parent_workflow_id.clone(),
        };
        self.next_seq = seq
            .checked_add(1)
            .ok_or(CorrelationError::SequenceOverflow { seq })?;
        Ok(envelope)
    }

    fn parent_workflow_id(&self) -> &WorkflowId {
        &self.parent_workflow_id
    }
}

/// Cancels each still-pending child, terminating its linked process and recording cancellation.
///
/// The caller's workflow process must trap exits before invoking this helper so the exit signal from
/// each killed link arrives as an ordinary mailbox message that can be drained by the collector.
///
/// # Errors
///
/// Returns [`CorrelationError`] if AE cannot terminate a linked child or AD cannot record a concrete
/// cancellation event through the parent workflow's recorder seam.
pub fn cancel_remaining(
    engine: &impl EngineHandle,
    recording: &mut CancellationRecordingContext,
    table: &mut CorrelatedResultTable,
    children: &[InFlightChild],
) -> Result<(), CorrelationError> {
    for child in children {
        if !table.is_pending(child.index) {
            continue;
        }

        let event = cancellation_event(recording, &child.child)?;
        engine.record_workflow_event(recording.parent_workflow_id(), event)?;
        table.mark_cancelled(child.index);
        terminate_linked_child(engine, recording.parent_workflow_id(), child)?;
    }
    Ok(())
}

fn terminate_linked_child(
    engine: &impl EngineHandle,
    parent_workflow_id: &WorkflowId,
    child: &InFlightChild,
) -> Result<(), EngineSeamError> {
    match &child.child {
        LinkedChild::Activity { process, .. } => {
            engine.terminate_linked_activity(parent_workflow_id, *process, child.token.value())
        }
        LinkedChild::Workflow { process, .. } => engine.terminate_linked_child_workflow(
            parent_workflow_id,
            *process,
            child.token.value(),
        ),
    }
}

fn cancellation_event(
    recording: &mut CancellationRecordingContext,
    child: &LinkedChild,
) -> Result<Event, CorrelationError> {
    match child {
        LinkedChild::Activity { activity_id, .. } => Ok(Event::ActivityCancelled {
            envelope: recording.next_envelope()?,
            activity_id: activity_id.clone(),
        }),
        LinkedChild::Workflow { workflow_id, .. } => Ok(Event::ChildWorkflowCancelled {
            envelope: recording.next_envelope()?,
            child_workflow_id: workflow_id.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{ContentType, Event, Payload, WorkflowId};
    use chrono::TimeZone as _;

    use super::*;
    use crate::engine_seam::test_support::{DeliveredWorkflowMessage, FakeEngineHandle};

    #[test]
    fn batch_tokens_are_distinct_and_reproducible() -> Result<(), Box<dyn std::error::Error>> {
        let first = derive_batch(42, 4)?;
        let second = derive_batch(42, 4)?;

        assert_eq!(first, second);
        assert_eq!(
            first.iter().map(|token| token.value()).collect::<Vec<_>>(),
            vec![42, 43, 44, 45]
        );
        assert_eq!(
            first.iter().collect::<std::collections::HashSet<_>>().len(),
            first.len()
        );
        Ok(())
    }

    #[test]
    fn same_type_results_are_routed_by_token_out_of_order() -> Result<(), Box<dyn std::error::Error>>
    {
        let batch = CorrelationBatch::from_base(100, 3)?;
        let mut table = CorrelatedResultTable::new(batch.clone());
        let child_a = WorkflowId::new_v4();
        let child_b = WorkflowId::new_v4();
        let child_c = WorkflowId::new_v4();
        let mut mailbox = VecCorrelationMailbox::new(vec![
            completed(child_c.clone(), batch.slots()[2].token(), "third"),
            completed(child_a.clone(), batch.slots()[0].token(), "first"),
            completed(child_b.clone(), batch.slots()[1].token(), "second"),
        ]);

        for expected_index in [2, 0, 1] {
            let result = mailbox.receive_correlated(&table)?;
            assert_eq!(result.index, expected_index);
            assert!(table.apply_result(result));
        }

        assert!(matches!(
            &table.states()[0],
            CorrelatedSlotState::Settled(CorrelatedOutcome::ChildWorkflowCompleted {
                child_workflow_id,
                result
            }) if child_workflow_id == &child_a && result == &payload("first")
        ));
        assert!(matches!(
            &table.states()[1],
            CorrelatedSlotState::Settled(CorrelatedOutcome::ChildWorkflowCompleted {
                child_workflow_id,
                result
            }) if child_workflow_id == &child_b && result == &payload("second")
        ));
        assert!(matches!(
            &table.states()[2],
            CorrelatedSlotState::Settled(CorrelatedOutcome::ChildWorkflowCompleted {
                child_workflow_id,
                result
            }) if child_workflow_id == &child_c && result == &payload("third")
        ));
        Ok(())
    }

    #[test]
    fn unrelated_messages_are_preserved_by_selective_receive()
    -> Result<(), Box<dyn std::error::Error>> {
        let batch = CorrelationBatch::from_base(200, 1)?;
        let table = CorrelatedResultTable::new(batch.clone());
        let signal_payload = payload("signal");
        let child = WorkflowId::new_v4();
        let mut mailbox = VecCorrelationMailbox::new(vec![
            WorkflowMailboxMessage::SignalReceived {
                name: "wake".to_owned(),
                payload: signal_payload.clone(),
            },
            completed(
                child.clone(),
                CorrelationToken::from_sequence_position(999),
                "other",
            ),
            completed(child.clone(), batch.slots()[0].token(), "match"),
        ]);

        let selected = mailbox.receive_correlated(&table)?;
        assert_eq!(selected.index, 0);
        assert_eq!(
            mailbox.delivered_messages(),
            vec![
                DeliveredWorkflowMessage::SignalReceived {
                    name: "wake".to_owned(),
                    payload: signal_payload,
                },
                DeliveredWorkflowMessage::ChildWorkflowCompleted {
                    child_workflow_id: child,
                    correlation: 999,
                    result: payload("other"),
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn cancel_remaining_terminates_and_records_each_pending_child()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let batch = CorrelationBatch::from_base(300, 3)?;
        let mut table = CorrelatedResultTable::new(batch.clone());
        table.apply_result(CorrelatedResult {
            index: 0,
            token: batch.slots()[0].token(),
            outcome: CorrelatedOutcome::ChildWorkflowCompleted {
                child_workflow_id: WorkflowId::new_v4(),
                result: payload("winner"),
            },
        });
        let child_workflow = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(77);
        let workflow_process = WorkflowProcessHandle::new(501);
        let activity_process = 601;
        let children = vec![
            InFlightChild::new(
                1,
                batch.slots()[1].token(),
                LinkedChild::Workflow {
                    workflow_id: child_workflow.clone(),
                    process: workflow_process,
                },
            ),
            InFlightChild::new(
                2,
                batch.slots()[2].token(),
                LinkedChild::Activity {
                    activity_id: activity_id.clone(),
                    process: activity_process,
                },
            ),
        ];
        let recorded_at = Utc
            .with_ymd_and_hms(2026, 6, 4, 13, 0, 0)
            .single()
            .ok_or("invalid test time")?;
        let mut recording = CancellationRecordingContext::new(parent.clone(), 10, recorded_at);

        cancel_remaining(&engine, &mut recording, &mut table, &children)?;

        assert_eq!(
            engine.terminated_child_workflows()?,
            vec![(
                parent.clone(),
                workflow_process,
                batch.slots()[1].token().value()
            )]
        );
        assert_eq!(
            engine.terminated_activities()?,
            vec![(
                parent.clone(),
                activity_process,
                batch.slots()[2].token().value()
            )]
        );
        let events = engine.recorded_events()?;
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            (workflow_id, Event::ChildWorkflowCancelled { envelope, child_workflow_id })
                if workflow_id == &parent && child_workflow_id == &child_workflow && envelope.seq == 10
        ));
        assert!(matches!(
            &events[1],
            (workflow_id, Event::ActivityCancelled { envelope, activity_id: cancelled })
                if workflow_id == &parent && cancelled == &activity_id && envelope.seq == 11
        ));
        assert!(matches!(table.states()[1], CorrelatedSlotState::Cancelled));
        assert!(matches!(table.states()[2], CorrelatedSlotState::Cancelled));
        Ok(())
    }

    #[test]
    fn late_result_after_cancellation_does_not_resurrect_slot()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let batch = CorrelationBatch::from_base(400, 1)?;
        let mut table = CorrelatedResultTable::new(batch.clone());
        let child_workflow = WorkflowId::new_v4();
        let child = InFlightChild::new(
            0,
            batch.slots()[0].token(),
            LinkedChild::Workflow {
                workflow_id: child_workflow.clone(),
                process: WorkflowProcessHandle::new(701),
            },
        );
        let recorded_at = Utc
            .with_ymd_and_hms(2026, 6, 4, 13, 5, 0)
            .single()
            .ok_or("invalid test time")?;
        let mut recording = CancellationRecordingContext::new(parent, 20, recorded_at);
        cancel_remaining(&engine, &mut recording, &mut table, &[child])?;

        let mut mailbox = VecCorrelationMailbox::new(vec![completed(
            child_workflow,
            batch.slots()[0].token(),
            "late",
        )]);
        assert_eq!(
            mailbox.receive_correlated(&table),
            Err(CorrelationError::MailboxClosed)
        );
        assert!(matches!(table.states()[0], CorrelatedSlotState::Cancelled));
        assert_eq!(mailbox.len(), 1);
        Ok(())
    }

    #[test]
    fn sequence_overflow_is_reported_without_recording_or_marking_cancelled()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let batch = CorrelationBatch::from_base(500, 1)?;
        let mut table = CorrelatedResultTable::new(batch.clone());
        let child = InFlightChild::new(
            0,
            batch.slots()[0].token(),
            LinkedChild::Workflow {
                workflow_id: WorkflowId::new_v4(),
                process: WorkflowProcessHandle::new(801),
            },
        );
        let recorded_at = Utc
            .with_ymd_and_hms(2026, 6, 4, 13, 10, 0)
            .single()
            .ok_or("invalid test time")?;
        let mut recording = CancellationRecordingContext::new(parent, u64::MAX, recorded_at);

        let result = cancel_remaining(&engine, &mut recording, &mut table, &[child]);

        assert_eq!(
            result,
            Err(CorrelationError::SequenceOverflow { seq: u64::MAX })
        );
        assert!(engine.recorded_events()?.is_empty());
        assert!(engine.terminated_child_workflows()?.is_empty());
        assert!(matches!(table.states()[0], CorrelatedSlotState::Pending));
        Ok(())
    }

    fn completed(
        child_workflow_id: WorkflowId,
        token: CorrelationToken,
        label: &str,
    ) -> WorkflowMailboxMessage {
        WorkflowMailboxMessage::ChildWorkflowCompleted {
            child_workflow_id,
            correlation: token.value(),
            result: payload(label),
        }
    }

    fn payload(label: &str) -> Payload {
        Payload::new(
            ContentType::Json,
            format!("{{\"label\":\"{label}\"}}").into_bytes(),
        )
    }
}
