//! all: fan-out, ordered collect, fail-fast
//!
//! `all` is a collector over AE-linked child workflows. Child completion/failure mailbox messages
//! are treated as already-recorded asynchronous observations; this module records child starts via
//! the child-spawn helper and records cancellation for any still-pending children on fail-fast.

use aion_core::{Payload, RunId, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

use crate::child::{ChildWorkflowError, ChildWorkflowRecordingContext, spawn};
use crate::concurrency::correlation::{
    CancellationRecordingContext, CorrelatedOutcome, CorrelatedResultTable, CorrelationBatch,
    CorrelationError, CorrelationMailbox, InFlightChild, LinkedChild, cancel_remaining,
};
use crate::engine_seam::EngineHandle;

/// A child workflow invocation spawned by [`all`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllChildWorkflowSpec {
    /// Child workflow type selected by the parent workflow.
    pub workflow_type: String,
    /// Opaque input passed to the child workflow.
    pub input: Payload,
    /// Concrete child run identifier requested from AE.
    pub run_id: RunId,
}

impl AllChildWorkflowSpec {
    /// Creates a child workflow spec for [`all`].
    #[must_use]
    pub fn new(workflow_type: impl Into<String>, input: Payload, run_id: RunId) -> Self {
        Self {
            workflow_type: workflow_type.into(),
            input,
            run_id,
        }
    }
}

/// Recording metadata used by [`all`] for child starts and fail-fast cancellation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllRecordingContext {
    parent_workflow_id: WorkflowId,
    base_sequence_position: u64,
    recorded_at: DateTime<Utc>,
}

impl AllRecordingContext {
    /// Creates recording metadata with the first sequence position used by this fan-out.
    #[must_use]
    pub const fn new(
        parent_workflow_id: WorkflowId,
        base_sequence_position: u64,
        recorded_at: DateTime<Utc>,
    ) -> Self {
        Self {
            parent_workflow_id,
            base_sequence_position,
            recorded_at,
        }
    }

    fn start_context_for(&self, index: usize) -> Result<ChildWorkflowRecordingContext, AllError> {
        let offset = u64::try_from(index).map_err(|_| AllError::SequenceOverflow {
            base_sequence_position: self.base_sequence_position,
            index,
        })?;
        let next_seq =
            self.base_sequence_position
                .checked_add(offset)
                .ok_or(AllError::SequenceOverflow {
                    base_sequence_position: self.base_sequence_position,
                    index,
                })?;
        Ok(ChildWorkflowRecordingContext::new(
            self.parent_workflow_id.clone(),
            next_seq,
            self.recorded_at,
        ))
    }

    fn cancellation_context(
        &self,
        len: usize,
        observed_outcomes: usize,
    ) -> Result<CancellationRecordingContext, AllError> {
        let after_starts = self.sequence_after_offset(len)?;
        let observed_offset =
            u64::try_from(observed_outcomes).map_err(|_| AllError::SequenceOverflow {
                base_sequence_position: self.base_sequence_position,
                index: len.saturating_add(observed_outcomes),
            })?;
        let next_seq = after_starts.checked_add(observed_offset).ok_or_else(|| {
            AllError::SequenceOverflow {
                base_sequence_position: self.base_sequence_position,
                index: len.saturating_add(observed_outcomes),
            }
        })?;
        Ok(CancellationRecordingContext::new(
            self.parent_workflow_id.clone(),
            next_seq,
            self.recorded_at,
        ))
    }

    fn sequence_after_offset(&self, index: usize) -> Result<u64, AllError> {
        let offset = u64::try_from(index).map_err(|_| AllError::SequenceOverflow {
            base_sequence_position: self.base_sequence_position,
            index,
        })?;
        self.base_sequence_position
            .checked_add(offset)
            .ok_or(AllError::SequenceOverflow {
                base_sequence_position: self.base_sequence_position,
                index,
            })
    }
}

/// Errors produced by the `all` concurrency primitive.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum AllError {
    /// Child spawn/start recording failed.
    #[error(transparent)]
    ChildWorkflow(#[from] ChildWorkflowError),
    /// Correlation matching or cancellation failed.
    #[error(transparent)]
    Correlation(#[from] CorrelationError),
    /// A child workflow failed, so `all` failed fast.
    #[error("child workflow {child_workflow_id} failed: {error}")]
    ChildFailed {
        /// Child workflow that failed.
        child_workflow_id: WorkflowId,
        /// Terminal child failure.
        error: WorkflowError,
    },
    /// A child workflow was cancelled while `all` was awaiting results.
    #[error("child workflow {child_workflow_id} was cancelled")]
    ChildCancelled {
        /// Child workflow that was cancelled.
        child_workflow_id: WorkflowId,
    },
    /// Sequence arithmetic overflowed while preparing start or cancellation events.
    #[error("all sequence overflow for base sequence {base_sequence_position} at index {index}")]
    SequenceOverflow {
        /// First sequence position in this fan-out.
        base_sequence_position: u64,
        /// Fan-out index that overflowed.
        index: usize,
    },
    /// Child spawning failed and cleanup of already-started children also failed.
    #[error("child spawn failed with {spawn_error}; cleanup also failed with {cleanup_error}")]
    SpawnCleanupFailed {
        /// Original spawn/start-recording failure.
        spawn_error: ChildWorkflowError,
        /// Failure observed while cancelling children that had already started.
        cleanup_error: CorrelationError,
    },
    /// The result table reported success without every input slot being populated.
    #[error("all completed without a result for input index {index}")]
    MissingResult {
        /// Missing input position.
        index: usize,
    },
}

/// Spawns all child workflow specs as linked children and returns their payloads in input order.
///
/// The base sequence position is used both for deterministic correlation tokens and for the
/// `ChildWorkflowStarted` events recorded during spawn. Outcome messages selected from `mailbox` are
/// assumed to have already been recorded by the async-arrival path before delivery to the parent
/// mailbox; this collector records only start and fail-fast cancellation observations.
///
/// # Errors
///
/// Returns [`AllError`] if spawning, matching, cancellation, or a child outcome fails.
pub fn all(
    engine: &impl EngineHandle,
    recording: &AllRecordingContext,
    mailbox: &mut impl CorrelationMailbox,
    specs: &[AllChildWorkflowSpec],
) -> Result<Vec<Payload>, AllError> {
    let batch = CorrelationBatch::from_base(recording.base_sequence_position, specs.len())?;
    let mut table = CorrelatedResultTable::new(batch.clone());
    let children = spawn_children(engine, recording, specs, &batch)?;
    let mut results = vec![None; specs.len()];
    let mut settled = 0_usize;

    while settled < specs.len() {
        let correlated = mailbox.receive_correlated(&table)?;
        let index = correlated.index;
        let outcome = correlated.outcome.clone();
        if !table.apply_result(correlated) {
            continue;
        }
        settled = settled.saturating_add(1);

        match outcome {
            CorrelatedOutcome::ChildWorkflowCompleted { result, .. } => {
                if let Some(slot) = results.get_mut(index) {
                    *slot = Some(result);
                }
            }
            CorrelatedOutcome::ChildWorkflowFailed {
                child_workflow_id,
                error,
            } => {
                cancel_in_flight(
                    engine,
                    recording,
                    specs.len(),
                    settled,
                    &mut table,
                    &children,
                )?;
                return Err(AllError::ChildFailed {
                    child_workflow_id,
                    error,
                });
            }
            CorrelatedOutcome::ChildWorkflowCancelled { child_workflow_id } => {
                cancel_in_flight(
                    engine,
                    recording,
                    specs.len(),
                    settled,
                    &mut table,
                    &children,
                )?;
                return Err(AllError::ChildCancelled { child_workflow_id });
            }
        }
    }

    ordered_results(results)
}

fn spawn_children(
    engine: &impl EngineHandle,
    recording: &AllRecordingContext,
    specs: &[AllChildWorkflowSpec],
    batch: &CorrelationBatch,
) -> Result<Vec<InFlightChild>, AllError> {
    let mut children = Vec::with_capacity(specs.len());
    for (spec, slot) in specs.iter().zip(batch.slots()) {
        let mut start_recording = recording.start_context_for(slot.index())?;
        let spawned = match spawn(
            engine,
            &mut start_recording,
            spec.workflow_type.clone(),
            spec.input.clone(),
            spec.run_id.clone(),
        ) {
            Ok(spawned) => spawned,
            Err(spawn_error) => {
                cleanup_started_after_spawn_failure(
                    engine,
                    recording,
                    batch,
                    &children,
                    spawn_error.clone(),
                )?;
                return Err(AllError::ChildWorkflow(spawn_error));
            }
        };
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

fn cleanup_started_after_spawn_failure(
    engine: &impl EngineHandle,
    recording: &AllRecordingContext,
    batch: &CorrelationBatch,
    children: &[InFlightChild],
    spawn_error: ChildWorkflowError,
) -> Result<(), AllError> {
    if children.is_empty() {
        return Ok(());
    }
    let mut table = CorrelatedResultTable::new(batch.clone());
    let mut cancellation = recording.cancellation_context(batch.len(), 0)?;
    cancel_remaining(engine, &mut cancellation, &mut table, children).map_err(|cleanup_error| {
        AllError::SpawnCleanupFailed {
            spawn_error,
            cleanup_error,
        }
    })
}

fn cancel_in_flight(
    engine: &impl EngineHandle,
    recording: &AllRecordingContext,
    len: usize,
    observed_outcomes: usize,
    table: &mut CorrelatedResultTable,
    children: &[InFlightChild],
) -> Result<(), AllError> {
    let mut cancellation = recording.cancellation_context(len, observed_outcomes)?;
    cancel_remaining(engine, &mut cancellation, table, children)?;
    Ok(())
}

fn ordered_results(results: Vec<Option<Payload>>) -> Result<Vec<Payload>, AllError> {
    let mut ordered = Vec::with_capacity(results.len());
    for (index, result) in results.into_iter().enumerate() {
        let payload = result.ok_or(AllError::MissingResult { index })?;
        ordered.push(payload);
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use aion_core::{ContentType, Event, Payload, RunId, WorkflowError, WorkflowId};
    use chrono::DateTime;

    use super::{AllChildWorkflowSpec, AllError, AllRecordingContext, all};
    use crate::concurrency::VecCorrelationMailbox;
    use crate::engine_seam::test_support::FakeEngineHandle;
    use crate::engine_seam::{
        ChildWorkflowSpawnMode, ChildWorkflowSpawnResult, EngineSeamError, WorkflowMailboxMessage,
        WorkflowProcessHandle,
    };

    fn payload(bytes: &'static [u8]) -> Payload {
        Payload::new(ContentType::Json, bytes.to_vec())
    }

    fn timestamp() -> Result<DateTime<chrono::Utc>, Box<dyn std::error::Error>> {
        Ok(DateTime::parse_from_rfc3339("2026-06-04T12:00:00Z").map(DateTime::from)?)
    }

    fn workflow_error(message: &str) -> WorkflowError {
        WorkflowError {
            message: message.to_owned(),
            details: None,
        }
    }

    fn spec(label: &'static [u8]) -> AllChildWorkflowSpec {
        AllChildWorkflowSpec::new("child", payload(label), RunId::new_v4())
    }

    fn queue_spawns(
        engine: &FakeEngineHandle,
        children: &[WorkflowId],
    ) -> Result<Vec<WorkflowProcessHandle>, Box<dyn std::error::Error>> {
        let mut processes = Vec::with_capacity(children.len());
        for (index, child) in children.iter().enumerate() {
            let pid = u64::try_from(index)?.saturating_add(10);
            let process = WorkflowProcessHandle::new(pid);
            processes.push(process);
            engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
                child_workflow_id: child.clone(),
                child_process: process,
            }))?;
        }
        Ok(processes)
    }

    #[test]
    fn all_collects_out_of_order_results_in_input_order() -> Result<(), Box<dyn std::error::Error>>
    {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let children = vec![
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        queue_spawns(&engine, &children)?;
        let specs = vec![
            spec(br#"{"input":0}"#),
            spec(br#"{"input":1}"#),
            spec(br#"{"input":2}"#),
        ];
        let recording = AllRecordingContext::new(parent.clone(), 40, timestamp()?);
        let result_a = payload(br#"{"result":0}"#);
        let result_b = payload(br#"{"result":1}"#);
        let result_c = payload(br#"{"result":2}"#);
        let mut mailbox = VecCorrelationMailbox::new(vec![
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: children[2].clone(),
                correlation: 42,
                result: result_c.clone(),
            },
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: children[0].clone(),
                correlation: 40,
                result: result_a.clone(),
            },
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: children[1].clone(),
                correlation: 41,
                result: result_b.clone(),
            },
        ]);

        let results = all(&engine, &recording, &mut mailbox, &specs)?;

        assert_eq!(results, vec![result_a, result_b, result_c]);
        assert!(mailbox.is_empty());
        let requests = engine.child_spawn_requests()?;
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.mode == ChildWorkflowSpawnMode::Linked)
        );
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 3);
        assert!(
            recorded
                .iter()
                .all(|(workflow_id, _)| workflow_id == &parent)
        );
        assert!(
            recorded
                .iter()
                .all(|(_, event)| matches!(event, Event::ChildWorkflowStarted { .. }))
        );
        Ok(())
    }

    #[test]
    fn all_fails_fast_and_cancels_pending_children() -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let children = vec![
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        let processes = queue_spawns(&engine, &children)?;
        let specs = vec![
            spec(br#"{"input":0}"#),
            spec(br#"{"input":1}"#),
            spec(br#"{"input":2}"#),
        ];
        let recording = AllRecordingContext::new(parent.clone(), 50, timestamp()?);
        let failure = workflow_error("boom");
        let mut mailbox =
            VecCorrelationMailbox::new(vec![WorkflowMailboxMessage::ChildWorkflowFailed {
                child_workflow_id: children[1].clone(),
                correlation: 51,
                error: failure.clone(),
            }]);

        let error = all(&engine, &recording, &mut mailbox, &specs);

        assert_eq!(
            error,
            Err(AllError::ChildFailed {
                child_workflow_id: children[1].clone(),
                error: failure,
            })
        );
        assert!(mailbox.is_empty());
        let terminated = engine.terminated_child_workflows()?;
        assert_eq!(
            terminated,
            vec![
                (parent.clone(), processes[0], 50),
                (parent.clone(), processes[2], 52)
            ]
        );
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 5);
        assert!(matches!(
            recorded[3].1,
            Event::ChildWorkflowCancelled { .. }
        ));
        assert_eq!(recorded[3].1.seq(), 54);
        assert!(matches!(
            recorded[4].1,
            Event::ChildWorkflowCancelled { .. }
        ));
        assert_eq!(recorded[4].1.seq(), 55);
        Ok(())
    }

    #[test]
    fn all_failure_after_prior_success_still_returns_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let children = vec![
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        let processes = queue_spawns(&engine, &children)?;
        let specs = vec![
            spec(br#"{"input":0}"#),
            spec(br#"{"input":1}"#),
            spec(br#"{"input":2}"#),
        ];
        let recording = AllRecordingContext::new(parent.clone(), 60, timestamp()?);
        let failure = workflow_error("late boom");
        let mut mailbox = VecCorrelationMailbox::new(vec![
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: children[0].clone(),
                correlation: 60,
                result: payload(br#"{"result":0}"#),
            },
            WorkflowMailboxMessage::ChildWorkflowFailed {
                child_workflow_id: children[2].clone(),
                correlation: 62,
                error: failure.clone(),
            },
        ]);

        let error = all(&engine, &recording, &mut mailbox, &specs);

        assert_eq!(
            error,
            Err(AllError::ChildFailed {
                child_workflow_id: children[2].clone(),
                error: failure,
            })
        );
        assert_eq!(
            engine.terminated_child_workflows()?,
            vec![(parent, processes[1], 61)]
        );
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 4);
        assert!(matches!(
            recorded[3].1,
            Event::ChildWorkflowCancelled { .. }
        ));
        assert_eq!(recorded[3].1.seq(), 65);
        Ok(())
    }

    #[test]
    fn all_propagates_spawn_error_without_returning_partial_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        engine.push_child_spawn_response(Err(EngineSeamError::ChildSpawn {
            reason: "capacity unavailable".to_owned(),
        }))?;
        let specs = vec![spec(br#"{"input":0}"#)];
        let recording = AllRecordingContext::new(parent, 80, timestamp()?);
        let mut mailbox = VecCorrelationMailbox::new(Vec::new());

        let error = all(&engine, &recording, &mut mailbox, &specs);

        assert_eq!(
            error,
            Err(AllError::ChildWorkflow(
                crate::child::ChildWorkflowError::Engine(EngineSeamError::ChildSpawn {
                    reason: "capacity unavailable".to_owned(),
                })
            ))
        );
        assert!(engine.recorded_events()?.is_empty());
        assert!(engine.terminated_child_workflows()?.is_empty());
        Ok(())
    }
}
