//! Worker protocol serde/prost wire types.

use crate::{ProtoActivityId, ProtoPayload, ProtoRunId, ProtoWorkflowId, WireError};

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
///
/// A registration scopes the worker to one `(namespace, task_queue)` worker
/// pool. The two dimensions are disjoint: `namespace` is the
/// correctness/isolation boundary the worker is authorized for, `task_queue`
/// is the pool/flavour selector within that namespace. `activity_type` (carried
/// in `activity_types`) is matched inside a pool, not used to select it.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoRegisterWorker {
    /// Correctness/isolation boundary this worker is authorized for.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Activity types implemented by the worker, preserving wire order.
    #[prost(string, repeated, tag = "2")]
    pub activity_types: Vec<String>,
    /// Pool/flavour selector within the namespace. The worker-pool address is
    /// `(namespace, task_queue)`; the server normalizes an empty value to the
    /// literal `"default"` pool.
    #[prost(string, tag = "3")]
    pub task_queue: String,
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
    /// One-based delivery attempt stamped by the dispatching engine seam.
    /// Zero is malformed: consumers reject a task whose attempt is 0 (the
    /// proto3 default means the producer failed to stamp it).
    #[prost(uint32, tag = "5")]
    pub attempt: u32,
    /// Human-meaningful display labels the workflow attached to the activity
    /// (for example `brief=IP-001`). Display metadata only — the engine never
    /// interprets them; they ride to the worker so its logs and the dashboard
    /// can show what a dispatch is working on. Empty when none were set.
    #[prost(map = "string, string", tag = "6")]
    pub labels: ::std::collections::HashMap<String, String>,
    /// Run that owns this dispatch; threaded so a completion only resolves the
    /// run that issued it (continue-as-new safety, OBX-011). Absent for legacy
    /// dispatches that predate run threading.
    #[prost(message, optional, tag = "7")]
    pub run_id: Option<ProtoRunId>,
}

/// Server-initiated drain: the server is going away (restart, deploy,
/// rebalance). The worker finishes already-assigned work, stops expecting
/// new tasks, and reconnects after the schedule's initial backoff. A drain
/// frame re-classifies the session's eventual stream end (clean or abrupt)
/// as a drain-class drop that consumes no drop budget — distinct from denial
/// (gRPC error status, terminal) and from an unannounced close (budgeted
/// retryable drop).
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoDrainRequest {}

/// Positive registration acknowledgement — always the first frame on the
/// response stream. There is no negative counterpart: a denied or invalid
/// registration fails the RPC with a gRPC error status.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoRegisterAck {
    /// Server-assigned stream identifier for this registration.
    #[prost(uint64, tag = "1")]
    pub worker_id: u64,
    /// The namespace the registration was authorized against.
    #[prost(string, tag = "2")]
    pub namespace: String,
    /// Operator-configured liveness window on this server, in milliseconds.
    #[prost(uint64, tag = "3")]
    pub heartbeat_window_ms: u64,
}

/// Per-result acknowledgement: the server has consumed the identified
/// `ActivityResult` frame and the worker may stop re-reporting it. Not a
/// durability receipt — the durable truth is the workflow's event history.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoResultAck {
    /// Owning workflow id.
    #[prost(message, optional, tag = "1")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Correlating activity id.
    #[prost(message, optional, tag = "2")]
    pub activity_id: Option<ProtoActivityId>,
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
    /// Run that owns this completion; echoed from the dispatched task so a
    /// completion only resolves the run that issued it (continue-as-new safety,
    /// OBX-011). Absent for legacy completions that predate run threading.
    #[prost(message, optional, tag = "5")]
    pub run_id: Option<ProtoRunId>,
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
        ProtoDrainRequest, ProtoHeartbeat, ProtoRegisterAck, ProtoRegisterWorker, ProtoResultAck,
        proto_activity_result,
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
            task_queue: String::from("claude"),
        };

        assert_json_and_proto_round_trip(&registration)
    }

    #[test]
    fn worker_registration_task_queue_uses_wire_tag_three() -> Result<(), Box<dyn std::error::Error>>
    {
        // Pins task_queue to proto tag 3 (field key 0x1A = tag 3,
        // length-delimited) so the hand-written stubs cannot drift, and
        // confirms an old-shape registration that omits it decodes to the
        // proto3 default `""` (the server normalizes that to "default").
        let registration = ProtoRegisterWorker {
            namespace: String::new(),
            activity_types: Vec::new(),
            task_queue: String::from("gpu"),
        };
        let mut bytes = Vec::new();
        registration.encode(&mut bytes)?;
        assert_eq!(bytes, vec![0x1A, 0x03, b'g', b'p', b'u']);

        // An encoded registration with no tag-3 field decodes task_queue to "".
        let no_task_queue = ProtoRegisterWorker {
            namespace: String::from("tenant-a"),
            activity_types: vec![String::from("charge-card")],
            task_queue: String::new(),
        };
        let mut bytes = Vec::new();
        no_task_queue.encode(&mut bytes)?;
        let decoded = ProtoRegisterWorker::decode(bytes.as_slice())?;
        assert_eq!(decoded.task_queue, "");
        Ok(())
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
            attempt: 3,
            labels: [
                (String::from("brief"), String::from("IP-001")),
                (String::from("repo"), String::from("ablative-io/yggdrasil")),
            ]
            .into_iter()
            .collect(),
            run_id: None,
        };

        assert_json_and_proto_round_trip(&task)
    }

    #[test]
    fn drain_request_round_trips_through_serde_and_proto() -> Result<(), Box<dyn std::error::Error>>
    {
        assert_json_and_proto_round_trip(&ProtoDrainRequest {})
    }

    #[test]
    fn register_ack_round_trips_through_serde_and_proto() -> Result<(), Box<dyn std::error::Error>>
    {
        let ack = ProtoRegisterAck {
            worker_id: 7,
            namespace: String::from("tenant-a"),
            heartbeat_window_ms: 30_000,
        };

        assert_json_and_proto_round_trip(&ack)
    }

    #[test]
    fn result_ack_round_trips_through_serde_and_proto() -> Result<(), Box<dyn std::error::Error>> {
        let ack = ProtoResultAck {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(
                aion_core::ActivityId::from_sequence_position(11),
            )),
        };

        assert_json_and_proto_round_trip(&ack)
    }

    #[cfg(feature = "generated")]
    #[test]
    fn server_to_worker_ack_arms_pin_oneof_tags_three_and_four()
    -> Result<(), Box<dyn std::error::Error>> {
        // Pins the new ServerToWorker oneof arms to wire tags 3 (register_ack)
        // and 4 (result_ack): field key = (tag << 3) | 2 (length-delimited).
        let register_ack = crate::generated::ServerToWorker {
            message: Some(crate::generated::server_to_worker::Message::RegisterAck(
                crate::generated::RegisterAck {
                    worker_id: 1,
                    namespace: String::from("tenant-a"),
                    heartbeat_window_ms: 1_000,
                },
            )),
        };
        let mut bytes = Vec::new();
        register_ack.encode(&mut bytes)?;
        assert_eq!(bytes.first(), Some(&0x1A));
        assert_eq!(
            crate::generated::ServerToWorker::decode(bytes.as_slice())?,
            register_ack
        );

        let result_ack = crate::generated::ServerToWorker {
            message: Some(crate::generated::server_to_worker::Message::ResultAck(
                crate::generated::ResultAck {
                    workflow_id: None,
                    activity_id: None,
                },
            )),
        };
        let mut bytes = Vec::new();
        result_ack.encode(&mut bytes)?;
        assert_eq!(bytes.first(), Some(&0x22));
        assert_eq!(
            crate::generated::ServerToWorker::decode(bytes.as_slice())?,
            result_ack
        );
        Ok(())
    }

    #[test]
    fn activity_task_attempt_uses_wire_tag_five() -> Result<(), Box<dyn std::error::Error>> {
        // Pins the attempt field to proto tag 5 (field key 0x28 = tag 5,
        // varint wire type) so the hand-written SDK stubs cannot drift.
        let task = ProtoActivityTask {
            workflow_id: None,
            activity_id: None,
            activity_type: String::new(),
            input: None,
            attempt: 9,
            labels: ::std::collections::HashMap::new(),
            run_id: None,
        };
        let mut bytes = Vec::new();
        task.encode(&mut bytes)?;
        assert_eq!(bytes, vec![0x28, 9]);
        Ok(())
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
            run_id: None,
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
            run_id: None,
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
