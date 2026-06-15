//! `ActivityTask` decode and `TaskResult`/`TaskFailure` encode.

use std::collections::BTreeMap;

use aion_core::{ActivityId, Payload, WorkflowId};
use aion_proto::ProtoActivityTask;

use crate::error::WorkerError;

/// SDK-level activity task envelope decoded from the AW-owned worker proto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityTask {
    /// Owning workflow id, required later when reporting this task's outcome.
    pub workflow_id: WorkflowId,
    /// Activity id correlating reports and heartbeats with this task.
    pub activity_id: ActivityId,
    /// Registered activity type name requested by the engine.
    pub activity_type: String,
    /// One-based delivery attempt stamped by the dispatching engine seam and
    /// read from the wire. Zero is malformed and rejected at decode.
    pub attempt: u32,
    /// Opaque activity input payload, preserving its content-type tag.
    pub input: Payload,
    /// Human-meaningful display labels the workflow attached to the activity
    /// (for example `brief=IP-001`). Display metadata only — surfaced in the
    /// worker's logs. `BTreeMap` keeps the rendered order stable; empty when
    /// the workflow attached none.
    pub labels: BTreeMap<String, String>,
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

        if value.attempt == 0 {
            // proto3 zero default = the producer failed to stamp the attempt.
            return Err(WorkerError::decode(MalformedActivityTask::MissingAttempt));
        }

        Ok(Self {
            workflow_id,
            activity_id,
            activity_type: value.activity_type,
            attempt: value.attempt,
            input,
            labels: value.labels.into_iter().collect(),
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
    #[error("activity task attempt is missing or zero (producer failed to stamp it)")]
    MissingAttempt,
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
            attempt: 3,
            labels: [(String::from("brief"), String::from("IP-001"))]
                .into_iter()
                .collect(),
        };

        let task = ActivityTask::try_from(proto)?;

        assert_eq!(task.workflow_id, workflow_id);
        assert_eq!(task.activity_id, activity_id);
        assert_eq!(task.activity_type, "charge-card");
        assert_eq!(task.attempt, 3, "attempt must be read from the wire");
        assert_eq!(task.input.content_type(), &ContentType::Json);
        assert_eq!(task.input.bytes(), input.bytes());
        assert_eq!(task.input.to_json()?, input_value);
        assert_eq!(
            task.labels.get("brief").map(String::as_str),
            Some("IP-001"),
            "display labels must decode from the wire"
        );
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
            attempt: 1,
            labels: std::collections::HashMap::new(),
        });

        assert!(matches!(result, Err(WorkerError::Decode { .. })));
    }

    #[test]
    fn zero_attempt_is_a_malformed_task() {
        let result = ActivityTask::try_from(ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(WorkflowId::new_v4())),
            activity_id: Some(ProtoActivityId::from(ActivityId::from_sequence_position(1))),
            activity_type: String::from("charge-card"),
            input: Some(ProtoPayload::from(Payload::new(
                ContentType::Json,
                b"{}".to_vec(),
            ))),
            attempt: 0,
            labels: std::collections::HashMap::new(),
        });

        let Err(error) = result else {
            unreachable!("attempt 0 must be rejected as malformed");
        };
        assert!(matches!(error, WorkerError::Decode { .. }));
        assert!(
            error.to_string().contains("attempt"),
            "error must name the attempt field: {error}"
        );
    }
}
