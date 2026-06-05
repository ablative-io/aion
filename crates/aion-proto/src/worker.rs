//! Worker protocol serde/prost wire types.

use crate::{ProtoActivityId, ProtoPayload, ProtoWorkflowId, WireError};

/// Proto representation of `ActivityErrorKind`. Zero is invalid on decode.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    prost::Enumeration,
)]
#[repr(i32)]
pub enum ProtoActivityErrorKind {
    /// Missing/invalid kind.
    Unspecified = 0,
    /// Activity failure may be retried by the engine.
    Retryable = 1,
    /// Activity failure is terminal.
    Terminal = 2,
}

/// Proto representation of `ActivityError`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoActivityError {
    /// Explicit retryability classification.
    #[prost(enumeration = "ProtoActivityErrorKind", tag = "1")]
    pub kind: i32,
    /// Human-readable error message.
    #[prost(string, tag = "2")]
    pub message: String,
    /// Optional structured failure details.
    #[prost(message, optional, tag = "3")]
    pub details: Option<ProtoPayload>,
}

/// Worker registration advertisement.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoRegisterWorker {
    /// Namespace that scopes this worker stream.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Activity types implemented by the worker, preserving wire order.
    #[prost(string, repeated, tag = "2")]
    pub activity_types: Vec<String>,
}

/// Activity invocation pushed to a worker.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoActivityTask {
    /// Owning workflow id.
    #[prost(message, optional, tag = "1")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Correlating activity id.
    #[prost(message, optional, tag = "2")]
    pub activity_id: Option<ProtoActivityId>,
    /// Activity type name.
    #[prost(string, tag = "3")]
    pub activity_type: String,
    /// Serialized activity input.
    #[prost(message, optional, tag = "4")]
    pub input: Option<ProtoPayload>,
}

/// Activity result or failure reported by a worker.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoActivityResult {
    /// Owning workflow id.
    #[prost(message, optional, tag = "1")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Correlating activity id.
    #[prost(message, optional, tag = "2")]
    pub activity_id: Option<ProtoActivityId>,
    /// Successful result payload or explicit activity error.
    #[prost(oneof = "proto_activity_result::Outcome", tags = "3, 4")]
    pub outcome: Option<proto_activity_result::Outcome>,
}

/// Types nested under [`ProtoActivityResult`].
pub mod proto_activity_result {
    use super::{ProtoActivityError, ProtoPayload};

    /// Proto oneof for activity success or failure.
    #[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Oneof)]
    pub enum Outcome {
        /// Successful activity output.
        #[prost(message, tag = "3")]
        Result(ProtoPayload),
        /// Activity failure preserving retryability classification.
        #[prost(message, tag = "4")]
        Error(ProtoActivityError),
    }
}

/// Worker heartbeat for an in-flight activity.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoHeartbeat {
    /// Owning workflow id.
    #[prost(message, optional, tag = "1")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Correlating activity id.
    #[prost(message, optional, tag = "2")]
    pub activity_id: Option<ProtoActivityId>,
    /// Optional opaque progress payload.
    #[prost(message, optional, tag = "3")]
    pub progress: Option<ProtoPayload>,
}

impl From<aion_core::ActivityErrorKind> for ProtoActivityErrorKind {
    fn from(value: aion_core::ActivityErrorKind) -> Self {
        match value {
            aion_core::ActivityErrorKind::Retryable => Self::Retryable,
            aion_core::ActivityErrorKind::Terminal => Self::Terminal,
        }
    }
}

impl TryFrom<ProtoActivityErrorKind> for aion_core::ActivityErrorKind {
    type Error = WireError;

    fn try_from(value: ProtoActivityErrorKind) -> Result<Self, Self::Error> {
        match value {
            ProtoActivityErrorKind::Unspecified => {
                Err(WireError::backend("activity error kind is missing"))
            }
            ProtoActivityErrorKind::Retryable => Ok(Self::Retryable),
            ProtoActivityErrorKind::Terminal => Ok(Self::Terminal),
        }
    }
}

impl From<aion_core::ActivityError> for ProtoActivityError {
    fn from(value: aion_core::ActivityError) -> Self {
        Self {
            kind: ProtoActivityErrorKind::from(value.kind) as i32,
            message: value.message,
            details: value.details.map(ProtoPayload::from),
        }
    }
}

impl TryFrom<ProtoActivityError> for aion_core::ActivityError {
    type Error = WireError;

    fn try_from(value: ProtoActivityError) -> Result<Self, Self::Error> {
        let kind = ProtoActivityErrorKind::try_from(value.kind)
            .map_err(|_| WireError::backend("activity error kind is unknown"))?;
        Ok(Self {
            kind: aion_core::ActivityErrorKind::try_from(kind)?,
            message: value.message,
            details: value
                .details
                .map(aion_core::Payload::try_from)
                .transpose()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use serde_json::json;

    use super::{
        ProtoActivityError, ProtoActivityErrorKind, ProtoActivityResult, ProtoActivityTask,
        ProtoHeartbeat, ProtoRegisterWorker, proto_activity_result,
    };
    use crate::{ProtoActivityId, ProtoPayload, ProtoWorkflowId, WireError};

    fn workflow_id() -> aion_core::WorkflowId {
        aion_core::WorkflowId::new(uuid::Uuid::nil())
    }

    #[test]
    fn activity_error_round_trips_preserving_classification() -> Result<(), WireError> {
        let core = aion_core::ActivityError {
            kind: aion_core::ActivityErrorKind::Retryable,
            message: String::from("connection reset"),
            details: Some(
                aion_core::Payload::from_json(&json!({"retry_after_ms": 500}))
                    .map_err(|_| WireError::backend("test payload could not be created"))?,
            ),
        };

        let proto = ProtoActivityError::from(core.clone());
        assert_eq!(aion_core::ActivityError::try_from(proto.clone())?, core);
        assert!(aion_core::ActivityError::try_from(proto)?.is_retryable());

        let terminal = ProtoActivityError {
            kind: ProtoActivityErrorKind::Terminal as i32,
            message: String::from("invalid request"),
            details: None,
        };
        assert!(!aion_core::ActivityError::try_from(terminal)?.is_retryable());

        Ok(())
    }

    #[test]
    fn worker_registration_round_trips_through_serde_and_proto()
    -> Result<(), Box<dyn std::error::Error>> {
        let registration = ProtoRegisterWorker {
            namespace: String::from("tenant-a"),
            activity_types: vec![String::from("charge-card"), String::from("send-email")],
        };

        assert_json_and_proto_round_trip(&registration)
    }

    #[test]
    fn activity_task_round_trips_through_serde_and_proto() -> Result<(), Box<dyn std::error::Error>>
    {
        let task = ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(
                aion_core::ActivityId::from_sequence_position(7),
            )),
            activity_type: String::from("charge-card"),
            input: Some(ProtoPayload::from(aion_core::Payload::from_json(
                &json!({"amount": 42}),
            )?)),
        };

        assert_json_and_proto_round_trip(&task)
    }

    #[test]
    fn activity_success_result_round_trips_through_serde_and_proto()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(
                aion_core::ActivityId::from_sequence_position(8),
            )),
            outcome: Some(proto_activity_result::Outcome::Result(ProtoPayload::from(
                aion_core::Payload::from_json(&json!({"authorization": "ok"}))?,
            ))),
        };

        assert_json_and_proto_round_trip(&result)
    }

    #[test]
    fn activity_error_result_round_trips_through_serde_and_proto()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(
                aion_core::ActivityId::from_sequence_position(9),
            )),
            outcome: Some(proto_activity_result::Outcome::Error(
                ProtoActivityError::from(aion_core::ActivityError {
                    kind: aion_core::ActivityErrorKind::Terminal,
                    message: String::from("card declined"),
                    details: Some(aion_core::Payload::from_json(&json!({"code": "declined"}))?),
                }),
            )),
        };

        assert_json_and_proto_round_trip(&result)
    }

    #[test]
    fn heartbeat_round_trips_through_serde_and_proto() -> Result<(), Box<dyn std::error::Error>> {
        let heartbeat = ProtoHeartbeat {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(
                aion_core::ActivityId::from_sequence_position(10),
            )),
            progress: Some(ProtoPayload::from(aion_core::Payload::from_json(
                &json!({"percent": 50}),
            )?)),
        };

        assert_json_and_proto_round_trip(&heartbeat)
    }

    fn assert_json_and_proto_round_trip<T>(value: &T) -> Result<(), Box<dyn std::error::Error>>
    where
        T: Message
            + Default
            + serde::Serialize
            + serde::de::DeserializeOwned
            + PartialEq
            + std::fmt::Debug,
    {
        assert_eq!(
            serde_json::from_str::<T>(&serde_json::to_string(value)?)?,
            *value
        );
        assert_eq!(prost_round_trip(value)?, *value);
        Ok(())
    }

    fn prost_round_trip<T>(value: &T) -> Result<T, Box<dyn std::error::Error>>
    where
        T: Message + Default,
    {
        let mut bytes = Vec::new();
        value.encode(&mut bytes)?;
        Ok(T::decode(bytes.as_slice())?)
    }
}
