//! Child workflow spawn mechanics over the AE engine seam.
//!
//! Spawning is record-then-spawn: the parent pre-allocates the child
//! workflow identifier, durably records `ChildWorkflowStarted` through its
//! own single Recorder, and only then asks AE to start the child under that
//! exact identifier. A crash between the record and the start leaves a
//! recoverable `ChildWorkflowStarted` (repaired by the engine's startup
//! recovery sweep) instead of an unrecorded duplicate-prone orphan.
//!
//! Children are not process-linked to their parents. Parent death leaves
//! children running, and awaited child terminals are observed by the
//! engine's child-terminal watcher, which records the parent-side
//! `ChildWorkflowCompleted`/`ChildWorkflowFailed` before waking the parent.

use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
use chrono::{DateTime, Utc};

use crate::engine_seam::{ChildWorkflowSpawnRequest, EngineHandle, EngineSeamError};

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
    /// Parent-allocated child workflow identity. The child has its own history.
    pub child_workflow_id: WorkflowId,
    /// AE live-process handle for the child execution.
    pub child_process: crate::engine_seam::WorkflowProcessHandle,
}

/// Errors produced by child workflow spawn operations.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ChildWorkflowError {
    /// AE or AD rejected a seam operation.
    #[error(transparent)]
    Engine(#[from] EngineSeamError),
}

/// Records `ChildWorkflowStarted` in the parent's history, then requests the child start.
///
/// The caller pre-allocates `child_workflow_id` (recorded nondeterminism:
/// drawn once on the live path, returned from history on replay) and it is
/// recorded durably *before* AE is asked to start the child, so the start
/// is exactly-once recoverable: replay resolves the spawn from the recorded
/// event, and a crash before the child process exists is repaired by the
/// startup recovery sweep from the same record.
///
/// # Errors
///
/// Returns [`ChildWorkflowError`] if the parent recorder rejects the start
/// event, AE cannot start the child, or AE starts the child under a
/// different identifier than the recorded one. A failure after the record
/// leaves the durable `ChildWorkflowStarted` in place by design.
pub fn spawn(
    engine: &impl EngineHandle,
    recording: &mut ChildWorkflowRecordingContext,
    child_type: impl Into<String>,
    input: Payload,
    child_workflow_id: WorkflowId,
    package_version: aion_core::PackageVersion,
) -> Result<SpawnedChildWorkflow, ChildWorkflowError> {
    let workflow_type = child_type.into();

    let event = Event::ChildWorkflowStarted {
        envelope: recording.next_envelope(),
        child_workflow_id: child_workflow_id.clone(),
        workflow_type: workflow_type.clone(),
        input: input.clone(),
        package_version: package_version.clone(),
    };
    engine.record_workflow_event(recording.parent_workflow_id(), event)?;

    let request = ChildWorkflowSpawnRequest {
        parent_workflow_id: recording.parent_workflow_id().clone(),
        child_workflow_id: child_workflow_id.clone(),
        workflow_type,
        input,
        package_version,
    };
    let result = engine.spawn_child_workflow(request)?;
    if result.child_workflow_id != child_workflow_id {
        return Err(ChildWorkflowError::Engine(EngineSeamError::ChildSpawn {
            reason: format!(
                "engine started child {} instead of the recorded id {child_workflow_id}",
                result.child_workflow_id
            ),
        }));
    }
    Ok(SpawnedChildWorkflow {
        child_workflow_id,
        child_process: result.child_process,
    })
}

#[cfg(test)]
mod tests {
    use aion_core::{ContentType, Event, Payload, WorkflowId};
    use chrono::DateTime;

    use super::{ChildWorkflowError, ChildWorkflowRecordingContext, spawn};
    use crate::engine_seam::test_support::{FakeEngineHandle, FakeEngineOperation};
    use crate::engine_seam::{ChildWorkflowSpawnResult, EngineSeamError, WorkflowProcessHandle};

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
    fn spawn_records_started_before_requesting_the_child_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        let input = payload(br#"{"item":1}"#);
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
            child.clone(),
            aion_core::PackageVersion::new("a".repeat(64)),
        )?;

        assert_eq!(spawned.child_workflow_id, child);
        assert_eq!(spawned.child_process, WorkflowProcessHandle::new(11));
        // #56 contract: the durable record precedes the spawn request, and
        // both carry the same pre-allocated child id.
        let operations = engine.operations()?;
        match (&operations[0], &operations[1]) {
            (
                FakeEngineOperation::EventRecorded { workflow_id, event },
                FakeEngineOperation::ChildSpawnRequested(request),
            ) => {
                assert_eq!(workflow_id, &parent);
                match event {
                    Event::ChildWorkflowStarted {
                        child_workflow_id,
                        workflow_type,
                        input: recorded_input,
                        ..
                    } => {
                        assert_eq!(child_workflow_id, &child);
                        assert_eq!(workflow_type, "child.worker");
                        assert_eq!(recorded_input, &input);
                    }
                    other => return Err(format!("unexpected event: {other:?}").into()),
                }
                assert_eq!(request.parent_workflow_id, parent);
                assert_eq!(request.child_workflow_id, child);
                assert_eq!(request.workflow_type, "child.worker");
                assert_eq!(request.input, input);
            }
            other => {
                return Err(format!("expected record-then-spawn order, found {other:?}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn spawn_failure_after_record_keeps_the_durable_start() -> Result<(), Box<dyn std::error::Error>>
    {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        engine.push_child_spawn_response(Err(EngineSeamError::ChildSpawn {
            reason: "engine declined".to_owned(),
        }))?;
        let mut recording = recording(parent)?;

        let observed = spawn(
            &engine,
            &mut recording,
            "child.worker",
            payload(b"null"),
            child.clone(),
            aion_core::PackageVersion::new("a".repeat(64)),
        );

        assert!(matches!(observed, Err(ChildWorkflowError::Engine(_))));
        // The recorded start survives the failed start request: this is the
        // crash-window record the recovery sweep repairs from.
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 1);
        assert!(matches!(
            &recorded[0].1,
            Event::ChildWorkflowStarted { child_workflow_id, .. } if child_workflow_id == &child
        ));
        Ok(())
    }

    #[test]
    fn spawn_rejects_engine_echoing_a_different_child_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
            child_workflow_id: WorkflowId::new_v4(),
            child_process: WorkflowProcessHandle::new(16),
        }))?;
        let mut recording = recording(parent)?;

        let observed = spawn(
            &engine,
            &mut recording,
            "child.worker",
            payload(b"null"),
            WorkflowId::new_v4(),
            aion_core::PackageVersion::new("a".repeat(64)),
        );

        match observed {
            Err(ChildWorkflowError::Engine(EngineSeamError::ChildSpawn { reason })) => {
                assert!(
                    reason.contains("instead of the recorded id"),
                    "unexpected reason: {reason}"
                );
            }
            other => return Err(format!("expected echo-mismatch failure: {other:?}").into()),
        }
        Ok(())
    }
}
