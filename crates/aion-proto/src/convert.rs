//! proto <-> aion-core conversions

use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::error::WireError;

const JSON_CONTENT_TYPE: &str = "application/json";

/// Proto representation of `WorkflowId`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoWorkflowId {
    /// UUID encoded in canonical string form.
    #[prost(string, tag = "1")]
    pub uuid: String,
}

/// Proto representation of `RunId`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoRunId {
    /// UUID encoded in canonical string form.
    #[prost(string, tag = "1")]
    pub uuid: String,
}

/// Proto representation of `ActivityId`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoActivityId {
    /// Scheduling sequence position.
    #[prost(uint64, tag = "1")]
    pub sequence_position: u64,
}

/// Proto representation of `TimerId`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoTimerId {
    /// Timer identifier kind.
    #[prost(oneof = "proto_timer_id::Kind", tags = "1, 2")]
    pub kind: Option<proto_timer_id::Kind>,
}

/// Types nested under [`ProtoTimerId`].
pub mod proto_timer_id {
    /// Proto oneof for named and anonymous timer identifiers.
    #[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Oneof)]
    pub enum Kind {
        /// Author-assigned timer name.
        #[prost(string, tag = "1")]
        Name(String),
        /// Engine-assigned timer sequence position.
        #[prost(uint64, tag = "2")]
        SequencePosition(u64),
    }
}

/// Proto representation of `Payload`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoPayload {
    /// Stable content type tag.
    #[prost(string, tag = "1")]
    pub content_type: String,
    /// Opaque serialized bytes.
    #[prost(bytes = "vec", tag = "2")]
    pub bytes: Vec<u8>,
}

/// Proto representation of `WorkflowStatus`. Zero is invalid on decode.
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
pub enum ProtoWorkflowStatus {
    /// Missing/invalid status.
    Unspecified = 0,
    /// Workflow is not terminal.
    Running = 1,
    /// Workflow completed successfully.
    Completed = 2,
    /// Workflow failed terminally.
    Failed = 3,
    /// Workflow was cancelled.
    Cancelled = 4,
    /// Workflow timed out.
    TimedOut = 5,
    /// Workflow continued as a new run.
    ContinuedAsNew = 6,
}

/// Thin proto envelope carrying a serde-encoded aion-core value.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct WireEnvelope {
    /// Namespace that scopes the enclosed value.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Optional caller request identifier.
    #[prost(string, optional, tag = "2")]
    pub request_id: Option<String>,
    /// Serialized aion-core value.
    #[prost(message, optional, tag = "3")]
    pub payload: Option<ProtoPayload>,
}

impl From<aion_core::WorkflowId> for ProtoWorkflowId {
    fn from(value: aion_core::WorkflowId) -> Self {
        Self {
            uuid: value.as_uuid().to_string(),
        }
    }
}

impl TryFrom<ProtoWorkflowId> for aion_core::WorkflowId {
    type Error = WireError;

    fn try_from(value: ProtoWorkflowId) -> Result<Self, Self::Error> {
        parse_uuid(&value.uuid, "workflow id").map(Self::new)
    }
}

impl From<aion_core::RunId> for ProtoRunId {
    fn from(value: aion_core::RunId) -> Self {
        Self {
            uuid: value.as_uuid().to_string(),
        }
    }
}

impl TryFrom<ProtoRunId> for aion_core::RunId {
    type Error = WireError;

    fn try_from(value: ProtoRunId) -> Result<Self, Self::Error> {
        parse_uuid(&value.uuid, "run id").map(Self::new)
    }
}

impl From<aion_core::ActivityId> for ProtoActivityId {
    fn from(value: aion_core::ActivityId) -> Self {
        Self {
            sequence_position: value.sequence_position(),
        }
    }
}

impl From<ProtoActivityId> for aion_core::ActivityId {
    fn from(value: ProtoActivityId) -> Self {
        Self::from_sequence_position(value.sequence_position)
    }
}

impl From<aion_core::TimerId> for ProtoTimerId {
    fn from(value: aion_core::TimerId) -> Self {
        let kind = if let Some(name) = value.name() {
            proto_timer_id::Kind::Name(String::from(name))
        } else if let Some(sequence_position) = value.sequence_position() {
            proto_timer_id::Kind::SequencePosition(sequence_position)
        } else {
            proto_timer_id::Kind::SequencePosition(0)
        };
        Self { kind: Some(kind) }
    }
}

impl TryFrom<ProtoTimerId> for aion_core::TimerId {
    type Error = WireError;

    fn try_from(value: ProtoTimerId) -> Result<Self, Self::Error> {
        match value.kind {
            Some(proto_timer_id::Kind::Name(name)) => aion_core::TimerId::named(name)
                .map_err(|_| WireError::backend("timer id name must not be empty")),
            Some(proto_timer_id::Kind::SequencePosition(sequence_position)) => {
                Ok(Self::anonymous(sequence_position))
            }
            None => Err(WireError::backend("timer id kind is missing")),
        }
    }
}

impl From<aion_core::Payload> for ProtoPayload {
    fn from(value: aion_core::Payload) -> Self {
        Self {
            content_type: content_type_to_wire(value.content_type()),
            bytes: value.bytes().to_vec(),
        }
    }
}

impl TryFrom<ProtoPayload> for aion_core::Payload {
    type Error = WireError;

    fn try_from(value: ProtoPayload) -> Result<Self, Self::Error> {
        let content_type = content_type_from_wire(&value.content_type)?;
        Ok(Self::new(content_type, value.bytes))
    }
}

impl From<aion_core::WorkflowStatus> for ProtoWorkflowStatus {
    fn from(value: aion_core::WorkflowStatus) -> Self {
        match value {
            aion_core::WorkflowStatus::Running => Self::Running,
            aion_core::WorkflowStatus::Completed => Self::Completed,
            aion_core::WorkflowStatus::Failed => Self::Failed,
            aion_core::WorkflowStatus::Cancelled => Self::Cancelled,
            aion_core::WorkflowStatus::TimedOut => Self::TimedOut,
            aion_core::WorkflowStatus::ContinuedAsNew => Self::ContinuedAsNew,
        }
    }
}

impl TryFrom<ProtoWorkflowStatus> for aion_core::WorkflowStatus {
    type Error = WireError;

    fn try_from(value: ProtoWorkflowStatus) -> Result<Self, Self::Error> {
        match value {
            ProtoWorkflowStatus::Unspecified => {
                Err(WireError::backend("workflow status is missing"))
            }
            ProtoWorkflowStatus::Running => Ok(Self::Running),
            ProtoWorkflowStatus::Completed => Ok(Self::Completed),
            ProtoWorkflowStatus::Failed => Ok(Self::Failed),
            ProtoWorkflowStatus::Cancelled => Ok(Self::Cancelled),
            ProtoWorkflowStatus::TimedOut => Ok(Self::TimedOut),
            ProtoWorkflowStatus::ContinuedAsNew => Ok(Self::ContinuedAsNew),
        }
    }
}

/// Serializes a core serde value into a thin wire envelope.
///
/// This helper is used for core `Event`, `WorkflowFilter`, and
/// `WorkflowSummary` values without declaring wire-clone structs for them.
///
/// # Errors
///
/// Returns [`WireError`] with code `backend` if the core value cannot be
/// serialized by serde JSON.
pub fn encode_core_value<T>(
    namespace: impl Into<String>,
    request_id: Option<String>,
    value: &T,
) -> Result<WireEnvelope, WireError>
where
    T: Serialize,
{
    let bytes =
        serde_json::to_vec(value).map_err(|_| WireError::backend("core value encode failed"))?;
    Ok(WireEnvelope {
        namespace: namespace.into(),
        request_id,
        payload: Some(ProtoPayload {
            content_type: String::from(JSON_CONTENT_TYPE),
            bytes,
        }),
    })
}

/// Deserializes a core serde value from a thin wire envelope.
///
/// Callers choose the target aion-core type, such as `Event`,
/// `WorkflowFilter`, or `WorkflowSummary`.
///
/// # Errors
///
/// Returns [`WireError`] with code `backend` if the envelope payload is
/// missing, uses an unknown content type, or cannot be deserialized as the
/// requested core type.
pub fn decode_core_value<T>(envelope: &WireEnvelope) -> Result<T, WireError>
where
    T: DeserializeOwned,
{
    let payload = envelope
        .payload
        .as_ref()
        .ok_or_else(|| WireError::backend("wire envelope payload is missing"))?;
    if payload.content_type != JSON_CONTENT_TYPE {
        return Err(WireError::backend("wire envelope content type is unknown"));
    }
    serde_json::from_slice(&payload.bytes)
        .map_err(|_| WireError::backend("core value decode failed"))
}

/// Serializes a workflow filter into a thin wire envelope.
///
/// # Errors
///
/// Returns [`WireError`] if the core filter cannot be serialized into the
/// envelope payload.
pub fn encode_workflow_filter(
    namespace: impl Into<String>,
    request_id: Option<String>,
    filter: &aion_core::WorkflowFilter,
) -> Result<WireEnvelope, WireError> {
    encode_core_value(namespace, request_id, filter)
}

/// Deserializes a workflow filter from a thin wire envelope.
///
/// # Errors
///
/// Returns [`WireError`] if the envelope is missing a payload, has an unknown
/// content type, or does not contain a valid workflow filter.
pub fn decode_workflow_filter(
    envelope: &WireEnvelope,
) -> Result<aion_core::WorkflowFilter, WireError> {
    decode_core_value(envelope)
}

/// Serializes a workflow summary into a thin wire envelope.
///
/// # Errors
///
/// Returns [`WireError`] if the core summary cannot be serialized into the
/// envelope payload.
pub fn encode_workflow_summary(
    namespace: impl Into<String>,
    request_id: Option<String>,
    summary: &aion_core::WorkflowSummary,
) -> Result<WireEnvelope, WireError> {
    encode_core_value(namespace, request_id, summary)
}

/// Deserializes a workflow summary from a thin wire envelope.
///
/// # Errors
///
/// Returns [`WireError`] if the envelope is missing a payload, has an unknown
/// content type, or does not contain a valid workflow summary.
pub fn decode_workflow_summary(
    envelope: &WireEnvelope,
) -> Result<aion_core::WorkflowSummary, WireError> {
    decode_core_value(envelope)
}

/// Serializes a workflow event into a thin wire envelope.
///
/// # Errors
///
/// Returns [`WireError`] if the core event cannot be serialized into the
/// envelope payload.
pub fn encode_event(
    namespace: impl Into<String>,
    request_id: Option<String>,
    event: &aion_core::Event,
) -> Result<WireEnvelope, WireError> {
    encode_core_value(namespace, request_id, event)
}

/// Deserializes a workflow event from a thin wire envelope.
///
/// # Errors
///
/// Returns [`WireError`] if the envelope is missing a payload, has an unknown
/// content type, or does not contain a valid workflow event.
pub fn decode_event(envelope: &WireEnvelope) -> Result<aion_core::Event, WireError> {
    decode_core_value(envelope)
}

fn parse_uuid(value: &str, label: &str) -> Result<Uuid, WireError> {
    Uuid::parse_str(value).map_err(|_| WireError::backend(format!("{label} uuid is malformed")))
}

fn content_type_to_wire(content_type: &aion_core::ContentType) -> String {
    match content_type {
        aion_core::ContentType::Json => String::from(JSON_CONTENT_TYPE),
    }
}

fn content_type_from_wire(content_type: &str) -> Result<aion_core::ContentType, WireError> {
    match content_type {
        JSON_CONTENT_TYPE => Ok(aion_core::ContentType::Json),
        _ => Err(WireError::backend("payload content type is unknown")),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{
        ProtoActivityId, ProtoPayload, ProtoRunId, ProtoTimerId, ProtoWorkflowId,
        ProtoWorkflowStatus, WireEnvelope, decode_core_value, encode_core_value, proto_timer_id,
    };
    use crate::error::WireError;

    fn workflow_id() -> aion_core::WorkflowId {
        aion_core::WorkflowId::new(uuid::Uuid::nil())
    }

    fn run_id() -> aion_core::RunId {
        aion_core::RunId::new(uuid::Uuid::nil())
    }

    fn recorded_at() -> Result<DateTime<Utc>, chrono::ParseError> {
        Ok(DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")?.with_timezone(&Utc))
    }

    fn event_envelope() -> Result<aion_core::EventEnvelope, chrono::ParseError> {
        Ok(aion_core::EventEnvelope {
            seq: 1,
            recorded_at: recorded_at()?,
            workflow_id: workflow_id(),
        })
    }

    #[test]
    fn workflow_id_round_trips_both_directions() -> Result<(), WireError> {
        let core = workflow_id();
        let proto = ProtoWorkflowId::from(core.clone());
        assert_eq!(aion_core::WorkflowId::try_from(proto.clone())?, core);
        assert_eq!(
            ProtoWorkflowId::from(aion_core::WorkflowId::try_from(proto)?),
            ProtoWorkflowId::from(core)
        );
        Ok(())
    }

    #[test]
    fn run_id_round_trips_both_directions() -> Result<(), WireError> {
        let core = run_id();
        let proto = ProtoRunId::from(core.clone());
        assert_eq!(aion_core::RunId::try_from(proto.clone())?, core);
        assert_eq!(
            ProtoRunId::from(aion_core::RunId::try_from(proto)?),
            ProtoRunId::from(core)
        );
        Ok(())
    }

    #[test]
    fn activity_id_round_trips_both_directions() {
        let core = aion_core::ActivityId::from_sequence_position(42);
        let proto = ProtoActivityId::from(core.clone());
        assert_eq!(aion_core::ActivityId::from(proto), core);
        assert_eq!(
            ProtoActivityId::from(aion_core::ActivityId::from(proto)),
            proto
        );
    }

    #[test]
    fn timer_id_round_trips_both_directions() -> Result<(), WireError> {
        let named = aion_core::TimerId::named("deadline")
            .map_err(|_| WireError::backend("test timer id could not be created"))?;
        let anonymous = aion_core::TimerId::anonymous(7);

        for core in [named, anonymous] {
            let proto = ProtoTimerId::from(core.clone());
            assert_eq!(aion_core::TimerId::try_from(proto.clone())?, core);
            assert_eq!(
                ProtoTimerId::from(aion_core::TimerId::try_from(proto)?),
                ProtoTimerId::from(core)
            );
        }

        Ok(())
    }

    #[test]
    fn timer_id_rejects_missing_and_empty_name() {
        let missing = ProtoTimerId { kind: None };
        assert_eq!(
            aion_core::TimerId::try_from(missing),
            Err(WireError::backend("timer id kind is missing"))
        );

        let empty = ProtoTimerId {
            kind: Some(proto_timer_id::Kind::Name(String::new())),
        };
        assert_eq!(
            aion_core::TimerId::try_from(empty),
            Err(WireError::backend("timer id name must not be empty"))
        );
    }

    #[test]
    fn payload_round_trips_all_json_kinds_and_raw_bytes() -> Result<(), WireError> {
        let values = [
            serde_json::Value::Null,
            json!(true),
            json!(123.45),
            json!("hello"),
            json!([null, false, 7, "item"]),
            json!({"nested": {"value": 1}, "array": [true, false]}),
        ];

        for value in values {
            let core = aion_core::Payload::from_json(&value)
                .map_err(|_| WireError::backend("test payload could not be created"))?;
            let proto = ProtoPayload::from(core.clone());
            assert_eq!(proto.content_type, "application/json");
            assert_eq!(proto.bytes, core.bytes());
            assert_eq!(aion_core::Payload::try_from(proto.clone())?, core);
            assert_eq!(
                ProtoPayload::from(aion_core::Payload::try_from(proto)?),
                ProtoPayload::from(core)
            );
        }

        let raw = aion_core::Payload::new(aion_core::ContentType::Json, vec![0, 159, 146, 150]);
        let proto = ProtoPayload::from(raw.clone());
        assert_eq!(proto.bytes, raw.bytes());
        assert_eq!(aion_core::Payload::try_from(proto)?, raw);
        Ok(())
    }

    #[test]
    fn workflow_status_round_trips_both_directions() -> Result<(), WireError> {
        let statuses = [
            aion_core::WorkflowStatus::Running,
            aion_core::WorkflowStatus::Completed,
            aion_core::WorkflowStatus::Failed,
            aion_core::WorkflowStatus::Cancelled,
            aion_core::WorkflowStatus::TimedOut,
            aion_core::WorkflowStatus::ContinuedAsNew,
        ];

        for core in statuses {
            let proto = ProtoWorkflowStatus::from(core);
            assert_eq!(aion_core::WorkflowStatus::try_from(proto)?, core);
            assert_eq!(
                ProtoWorkflowStatus::from(aion_core::WorkflowStatus::try_from(proto)?),
                proto
            );
        }

        assert_eq!(
            aion_core::WorkflowStatus::try_from(ProtoWorkflowStatus::Unspecified),
            Err(WireError::backend("workflow status is missing"))
        );
        Ok(())
    }

    #[test]
    fn core_event_round_trips_through_wire_envelope() -> Result<(), Box<dyn std::error::Error>> {
        let event = aion_core::Event::WorkflowStarted {
            envelope: event_envelope()?,
            workflow_type: String::from("checkout"),
            input: aion_core::Payload::from_json(&json!({ "cart": ["sku-1"] }))?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
        };

        let envelope = encode_core_value("tenant-a", Some(String::from("request-1")), &event)?;
        assert_eq!(envelope.namespace, "tenant-a");
        assert_eq!(envelope.request_id.as_deref(), Some("request-1"));

        let decoded: aion_core::Event = decode_core_value(&envelope)?;
        assert_eq!(decoded, event);
        Ok(())
    }

    #[test]
    fn envelope_rejects_missing_payload() {
        let envelope = WireEnvelope {
            namespace: String::from("tenant-a"),
            request_id: None,
            payload: None,
        };

        let decoded = decode_core_value::<aion_core::Event>(&envelope);
        assert_eq!(
            decoded,
            Err(WireError::backend("wire envelope payload is missing"))
        );
    }
}
