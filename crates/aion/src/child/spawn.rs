//! Child workflow spawn, await, and fire-and-forget mechanics.
//!
//! Awaited child workflows are requested as AE-owned linked processes. The link is established by
//! AE from the [`ChildWorkflowSpawnMode::Linked`] spawn request, so parent cancellation or
//! termination propagates to the linked child as an exit signal without AT owning teardown or
//! supervision. Fire-and-forget children are requested in monitor mode and this module records only
//! the parent-side start observation before returning.

use std::collections::VecDeque;

use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

use crate::engine_seam::{
    ChildWorkflowSpawnMode, ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle,
    EngineSeamError, WorkflowMailboxMessage,
};

/// Metadata used to envelope child-workflow events before routing them through the recorder seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildWorkflowRecordingContext {
    parent_workflow_id: WorkflowId,
    next_seq: u64,
    recorded_at: DateTime<Utc>,
}

impl ChildWorkflowRecordingContext {
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

    fn next_envelope(&mut self) -> EventEnvelope {
        let envelope = EventEnvelope {
            seq: self.next_seq,
            recorded_at: self.recorded_at,
            workflow_id: self.parent_workflow_id.clone(),
        };
        self.next_seq = self.next_seq.saturating_add(1);
        envelope
    }

    fn parent_workflow_id(&self) -> &WorkflowId {
        &self.parent_workflow_id
    }
}

/// Result returned after AE accepts a child-workflow spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnedChildWorkflow {
    /// AE-created child workflow identity. The child has its own history.
    pub child_workflow_id: WorkflowId,
    /// AE live-process handle for the child execution.
    pub child_process: crate::engine_seam::WorkflowProcessHandle,
}

impl From<ChildWorkflowSpawnResult> for SpawnedChildWorkflow {
    fn from(result: ChildWorkflowSpawnResult) -> Self {
        Self {
            child_workflow_id: result.child_workflow_id,
            child_process: result.child_process,
        }
    }
}

/// Errors produced by child workflow spawn/await operations.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ChildWorkflowError {
    /// AE or AD rejected a seam operation.
    #[error(transparent)]
    Engine(#[from] EngineSeamError),
    /// The awaited child failed terminally.
    #[error("child workflow {child_workflow_id} failed: {error}")]
    Failed {
        /// Child workflow that failed.
        child_workflow_id: WorkflowId,
        /// Terminal child failure.
        error: WorkflowError,
    },
    /// The parent mailbox closed before the awaited child produced an outcome.
    #[error("parent mailbox closed before child workflow {child_workflow_id} completed")]
    MailboxClosed {
        /// Awaited child workflow.
        child_workflow_id: WorkflowId,
    },
}

/// Parent mailbox abstraction used by `spawn_and_wait` selective receive.
pub trait ChildWorkflowMailbox {
    /// Blocks until a child workflow outcome message is available.
    ///
    /// # Errors
    ///
    /// Returns [`ChildWorkflowError`] if the mailbox cannot yield another message.
    fn receive_child_workflow_message(
        &mut self,
    ) -> Result<WorkflowMailboxMessage, ChildWorkflowError>;

    /// Puts an unrelated message back so the parent does not consume arbitrary mailbox traffic.
    ///
    /// # Errors
    ///
    /// Returns [`ChildWorkflowError`] if the mailbox cannot restore the unrelated message.
    fn requeue_unrelated(
        &mut self,
        message: WorkflowMailboxMessage,
    ) -> Result<(), ChildWorkflowError>;
}

/// Simple FIFO mailbox useful for tests and embedding contexts that already drained messages.
#[derive(Clone, Debug, Default)]
pub struct VecChildWorkflowMailbox {
    messages: VecDeque<WorkflowMailboxMessage>,
}

impl VecChildWorkflowMailbox {
    /// Creates a FIFO mailbox from an ordered message list.
    #[must_use]
    pub fn new(messages: Vec<WorkflowMailboxMessage>) -> Self {
        Self {
            messages: VecDeque::from(messages),
        }
    }

    /// Returns the currently queued messages in receive order.
    #[must_use]
    pub fn messages(&self) -> Vec<WorkflowMailboxMessage> {
        self.messages.iter().cloned().collect()
    }
}

impl ChildWorkflowMailbox for VecChildWorkflowMailbox {
    fn receive_child_workflow_message(
        &mut self,
    ) -> Result<WorkflowMailboxMessage, ChildWorkflowError> {
        self.messages
            .pop_front()
            .ok_or_else(|| ChildWorkflowError::MailboxClosed {
                child_workflow_id: WorkflowId::new_v4(),
            })
    }

    fn requeue_unrelated(
        &mut self,
        message: WorkflowMailboxMessage,
    ) -> Result<(), ChildWorkflowError> {
        self.messages.push_back(message);
        Ok(())
    }
}

/// Requests a linked child workflow and records `ChildWorkflowStarted` in the parent's history.
///
/// Parent cancellation propagates to this child through the AE-established process link.
///
/// # Errors
///
/// Returns [`ChildWorkflowError`] if AE cannot spawn the child or the parent recorder rejects the
/// start event.
pub fn spawn(
    engine: &impl EngineHandle,
    recording: &mut ChildWorkflowRecordingContext,
    child_type: impl Into<String>,
    input: Payload,
    run_id: RunId,
) -> Result<SpawnedChildWorkflow, ChildWorkflowError> {
    spawn_with_mode(
        engine,
        recording,
        child_type,
        input,
        run_id,
        ChildWorkflowSpawnMode::Linked,
    )
}

/// Requests a monitored fire-and-forget child, records `ChildWorkflowStarted`, and returns without
/// awaiting a result.
///
/// # Errors
///
/// Returns [`ChildWorkflowError`] if AE cannot spawn the child or the parent recorder rejects the
/// start event.
pub fn spawn_fire_and_forget(
    engine: &impl EngineHandle,
    recording: &mut ChildWorkflowRecordingContext,
    child_type: impl Into<String>,
    input: Payload,
    run_id: RunId,
) -> Result<SpawnedChildWorkflow, ChildWorkflowError> {
    spawn_with_mode(
        engine,
        recording,
        child_type,
        input,
        run_id,
        ChildWorkflowSpawnMode::DetachedMonitor,
    )
}

/// Spawns a linked child workflow and waits for the matching child result message.
///
/// # Errors
///
/// Returns [`ChildWorkflowError::Failed`] after recording `ChildWorkflowFailed` when the child
/// reports terminal failure. Other variants report seam or mailbox failures.
pub fn spawn_and_wait(
    engine: &impl EngineHandle,
    recording: &mut ChildWorkflowRecordingContext,
    mailbox: &mut impl ChildWorkflowMailbox,
    child_type: impl Into<String>,
    input: Payload,
    run_id: RunId,
) -> Result<Payload, ChildWorkflowError> {
    let child = spawn(engine, recording, child_type, input, run_id)?;
    wait_for_child_result(engine, recording, mailbox, &child.child_workflow_id)
}

fn spawn_with_mode(
    engine: &impl EngineHandle,
    recording: &mut ChildWorkflowRecordingContext,
    child_type: impl Into<String>,
    input: Payload,
    run_id: RunId,
    mode: ChildWorkflowSpawnMode,
) -> Result<SpawnedChildWorkflow, ChildWorkflowError> {
    let workflow_type = child_type.into();
    let request = ChildWorkflowSpawnRequest {
        parent_workflow_id: recording.parent_workflow_id().clone(),
        workflow_type: workflow_type.clone(),
        input: input.clone(),
        run_id,
        mode,
    };
    let child = SpawnedChildWorkflow::from(engine.spawn_child_workflow(request)?);
    let event = Event::ChildWorkflowStarted {
        envelope: recording.next_envelope(),
        child_workflow_id: child.child_workflow_id.clone(),
        workflow_type,
        input,
    };
    engine.record_workflow_event(recording.parent_workflow_id(), event)?;
    Ok(child)
}

fn wait_for_child_result(
    engine: &impl EngineHandle,
    recording: &mut ChildWorkflowRecordingContext,
    mailbox: &mut impl ChildWorkflowMailbox,
    child_workflow_id: &WorkflowId,
) -> Result<Payload, ChildWorkflowError> {
    loop {
        let message = mailbox.receive_child_workflow_message().map_err(|error| {
            if matches!(error, ChildWorkflowError::MailboxClosed { .. }) {
                ChildWorkflowError::MailboxClosed {
                    child_workflow_id: child_workflow_id.clone(),
                }
            } else {
                error
            }
        })?;
        match message {
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: completed_child,
                result,
                ..
            } if completed_child == *child_workflow_id => {
                let event = Event::ChildWorkflowCompleted {
                    envelope: recording.next_envelope(),
                    child_workflow_id: completed_child,
                    result: result.clone(),
                };
                engine.record_workflow_event(recording.parent_workflow_id(), event)?;
                return Ok(result);
            }
            WorkflowMailboxMessage::ChildWorkflowFailed {
                child_workflow_id: failed_child,
                error,
                ..
            } if failed_child == *child_workflow_id => {
                let event = Event::ChildWorkflowFailed {
                    envelope: recording.next_envelope(),
                    child_workflow_id: failed_child.clone(),
                    error: error.clone(),
                };
                engine.record_workflow_event(recording.parent_workflow_id(), event)?;
                return Err(ChildWorkflowError::Failed {
                    child_workflow_id: failed_child,
                    error,
                });
            }
            other => mailbox.requeue_unrelated(other)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{ContentType, Event, Payload, RunId, WorkflowError, WorkflowId};
    use chrono::DateTime;

    use super::{
        ChildWorkflowError, ChildWorkflowRecordingContext, VecChildWorkflowMailbox, spawn,
        spawn_and_wait, spawn_fire_and_forget,
    };
    use crate::engine_seam::test_support::{FakeEngineHandle, FakeEngineOperation};
    use crate::engine_seam::{
        ChildWorkflowSpawnMode, ChildWorkflowSpawnResult, WorkflowMailboxMessage,
        WorkflowProcessHandle,
    };

    fn payload(bytes: &'static [u8]) -> Payload {
        Payload::new(ContentType::Json, bytes.to_vec())
    }

    fn recording(
        parent: WorkflowId,
    ) -> Result<ChildWorkflowRecordingContext, Box<dyn std::error::Error>> {
        let recorded_at =
            DateTime::parse_from_rfc3339("2026-06-04T12:00:00Z").map(DateTime::from)?;
        Ok(ChildWorkflowRecordingContext::new(parent, 7, recorded_at))
    }

    #[test]
    fn spawn_requests_linked_child_and_records_started() -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        let input = payload(br#"{"item":1}"#);
        let run_id = RunId::new_v4();
        engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
            child_workflow_id: child.clone(),
            child_process: WorkflowProcessHandle::new(11),
        }))?;
        let mut recording = recording(parent.clone())?;

        let spawned = spawn(
            &engine,
            &mut recording,
            "child.worker",
            input.clone(),
            run_id.clone(),
        )?;

        assert_eq!(spawned.child_workflow_id, child);
        let requests = engine.child_spawn_requests()?;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].parent_workflow_id, parent);
        assert_eq!(requests[0].workflow_type, "child.worker");
        assert_eq!(requests[0].input, input);
        assert_eq!(requests[0].run_id, run_id);
        assert_eq!(requests[0].mode, ChildWorkflowSpawnMode::Linked);
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, parent);
        match &recorded[0].1 {
            Event::ChildWorkflowStarted {
                child_workflow_id,
                workflow_type,
                input: recorded_input,
                ..
            } => {
                assert_eq!(child_workflow_id, &spawned.child_workflow_id);
                assert_eq!(workflow_type, "child.worker");
                assert_eq!(recorded_input, &input);
            }
            other => return Err(format!("unexpected event: {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn spawn_and_wait_records_completion_and_returns_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        let result = payload(br#"{"ok":true}"#);
        engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
            child_workflow_id: child.clone(),
            child_process: WorkflowProcessHandle::new(12),
        }))?;
        let unrelated = WorkflowMailboxMessage::SignalReceived {
            name: "later".to_owned(),
            payload: payload(b"null"),
        };
        let mut mailbox = VecChildWorkflowMailbox::new(vec![
            unrelated.clone(),
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: child.clone(),
                correlation: 99,
                result: result.clone(),
            },
        ]);
        let mut recording = recording(parent.clone())?;

        let observed = spawn_and_wait(
            &engine,
            &mut recording,
            &mut mailbox,
            "child.worker",
            payload(b"null"),
            RunId::new_v4(),
        )?;

        assert_eq!(observed, result);
        assert_eq!(mailbox.messages(), vec![unrelated]);
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 2);
        assert!(matches!(
            recorded[1].1,
            Event::ChildWorkflowCompleted { .. }
        ));
        match &recorded[1].1 {
            Event::ChildWorkflowCompleted {
                child_workflow_id,
                result: recorded_result,
                ..
            } => {
                assert_eq!(child_workflow_id, &child);
                assert_eq!(recorded_result, &result);
            }
            other => return Err(format!("unexpected event: {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn spawn_and_wait_records_failure_and_surfaces_typed_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        let error = WorkflowError {
            message: "child failed".to_owned(),
            details: None,
        };
        engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
            child_workflow_id: child.clone(),
            child_process: WorkflowProcessHandle::new(13),
        }))?;
        let mut mailbox =
            VecChildWorkflowMailbox::new(vec![WorkflowMailboxMessage::ChildWorkflowFailed {
                child_workflow_id: child.clone(),
                correlation: 7,
                error: error.clone(),
            }]);
        let mut recording = recording(parent)?;

        let observed = spawn_and_wait(
            &engine,
            &mut recording,
            &mut mailbox,
            "child.worker",
            payload(b"null"),
            RunId::new_v4(),
        );

        assert_eq!(
            observed,
            Err(ChildWorkflowError::Failed {
                child_workflow_id: child.clone(),
                error: error.clone(),
            })
        );
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 2);
        match &recorded[1].1 {
            Event::ChildWorkflowFailed {
                child_workflow_id,
                error: recorded_error,
                ..
            } => {
                assert_eq!(child_workflow_id, &child);
                assert_eq!(recorded_error, &error);
            }
            other => return Err(format!("unexpected event: {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn fire_and_forget_records_start_and_returns_without_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
            child_workflow_id: child,
            child_process: WorkflowProcessHandle::new(14),
        }))?;
        let mut recording = recording(parent)?;

        let spawned = spawn_fire_and_forget(
            &engine,
            &mut recording,
            "child.worker",
            payload(b"null"),
            RunId::new_v4(),
        )?;

        assert_eq!(spawned.child_process, WorkflowProcessHandle::new(14));
        assert_eq!(engine.recorded_events()?.len(), 1);
        assert_eq!(
            engine.child_spawn_requests()?[0].mode,
            ChildWorkflowSpawnMode::DetachedMonitor
        );
        Ok(())
    }

    #[test]
    fn parent_termination_propagates_exit_to_linked_child() -> Result<(), Box<dyn std::error::Error>>
    {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
            child_workflow_id: child.clone(),
            child_process: WorkflowProcessHandle::new(15),
        }))?;
        let mut recording = recording(parent.clone())?;

        spawn(
            &engine,
            &mut recording,
            "child.worker",
            payload(b"null"),
            RunId::new_v4(),
        )?;
        engine.terminate_parent(&parent)?;

        assert_eq!(engine.propagated_child_exits()?, vec![(parent, child)]);
        assert!(matches!(
            engine.operations()?.first(),
            Some(FakeEngineOperation::ChildSpawnRequested(_))
        ));
        Ok(())
    }
}
