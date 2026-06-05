//! `ActivityTask` decode and `TaskResult`/`TaskFailure` encode.

use aion_core::{ActivityId, Payload, WorkflowId};
use aion_proto::ProtoActivityTask;

use crate::error::WorkerError;

const WIRE_DEFAULT_ATTEMPT: u32 = 1;

/// SDK-level activity task envelope decoded from the AW-owned worker proto.
///
/// The current worker wire shape does not carry an attempt field. The SDK keeps
/// an attempt property for the worker-side API and reports the first-attempt
/// value until AW adds an owned wire field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityTask {
    /// Owning workflow id, required later when reporting this task's outcome.
    pub workflow_id: WorkflowId,
    /// Activity id correlating reports and heartbeats with this task.
    pub activity_id: ActivityId,
    /// Registered activity type name requested by the engine.
    pub activity_type: String,
    /// Attempt number surfaced to execution machinery.
    pub attempt: u32,
    /// Opaque activity input payload, preserving its content-type tag.
    pub input: Payload,
}

impl TryFrom<ProtoActivityTask> for ActivityTask {
    type Error = WorkerError;

    fn try_from(value: ProtoActivityTask) -> Result<Self, Self::Error> {
        let workflow_id = value
            .workflow_id
            .ok_or(MalformedActivityTask::MissingWorkflowId)
            .and_then(|workflow_id| {
                WorkflowId::try_from(workflow_id)
                    .map_err(|source| MalformedActivityTask::InvalidWorkflowId { source })
            })
            .map_err(WorkerError::decode)?;
        let activity_id = value
            .activity_id
            .ok_or(MalformedActivityTask::MissingActivityId)
            .map(ActivityId::from)
            .map_err(WorkerError::decode)?;
        if value.activity_type.is_empty() {
            return Err(WorkerError::decode(
                MalformedActivityTask::MissingActivityType,
            ));
        }
        let input = value
            .input
            .ok_or(MalformedActivityTask::MissingInput)
            .and_then(|input| {
                Payload::try_from(input)
                    .map_err(|source| MalformedActivityTask::InvalidInput { source })
            })
            .map_err(WorkerError::decode)?;

        Ok(Self {
            workflow_id,
            activity_id,
            activity_type: value.activity_type,
            attempt: WIRE_DEFAULT_ATTEMPT,
            input,
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum MalformedActivityTask {
    #[error("activity task workflow_id is missing")]
    MissingWorkflowId,
    #[error("activity task workflow_id is invalid: {source}")]
    InvalidWorkflowId { source: aion_proto::WireError },
    #[error("activity task activity_id is missing")]
    MissingActivityId,
    #[error("activity task activity_type is missing")]
    MissingActivityType,
    #[error("activity task input payload is missing")]
    MissingInput,
    #[error("activity task input payload is invalid: {source}")]
    InvalidInput { source: aion_proto::WireError },
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityId, ContentType, Payload, WorkflowId};
    use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
    use serde_json::json;

    use super::ActivityTask;
    use crate::WorkerError;

    #[test]
    fn decodes_proto_activity_task_preserving_payload_content_type()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(42);
        let input_value = json!({"amount": 1250, "currency": "USD"});
        let input = Payload::from_json(&input_value)?;
        let proto = ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
            activity_id: Some(ProtoActivityId::from(activity_id.clone())),
            activity_type: String::from("charge-card"),
            input: Some(ProtoPayload::from(input.clone())),
        };

        let task = ActivityTask::try_from(proto)?;

        assert_eq!(task.workflow_id, workflow_id);
        assert_eq!(task.activity_id, activity_id);
        assert_eq!(task.activity_type, "charge-card");
        assert_eq!(task.attempt, 1);
        assert_eq!(task.input.content_type(), &ContentType::Json);
        assert_eq!(task.input.bytes(), input.bytes());
        assert_eq!(task.input.to_json()?, input_value);
        Ok(())
    }

    #[test]
    fn missing_required_field_maps_to_decode_error() {
        let result = ActivityTask::try_from(ProtoActivityTask {
            workflow_id: None,
            activity_id: Some(ProtoActivityId::from(ActivityId::from_sequence_position(1))),
            activity_type: String::from("charge-card"),
            input: Some(ProtoPayload::from(Payload::new(
                ContentType::Json,
                b"{}".to_vec(),
            ))),
        });

        assert!(matches!(result, Err(WorkerError::Decode { .. })));
    }
}
