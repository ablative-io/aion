//! Schedule-management serde/prost wire types.

use crate::convert::{ProtoScheduleId, WireEnvelope};

/// Proto representation of `CreateScheduleRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoCreateScheduleRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Serde-encoded `aion_core::ScheduleConfig` envelope.
    #[prost(message, optional, tag = "2")]
    pub config: Option<WireEnvelope>,
}

/// Proto representation of `CreateScheduleResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoCreateScheduleResponse {
    /// Assigned schedule identifier.
    #[prost(message, optional, tag = "1")]
    pub schedule_id: Option<ProtoScheduleId>,
    /// Serde-encoded `aion::schedule::ScheduleState` envelope.
    #[prost(message, optional, tag = "2")]
    pub state: Option<WireEnvelope>,
}

/// Proto representation of `UpdateScheduleRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoUpdateScheduleRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Target schedule identifier.
    #[prost(message, optional, tag = "2")]
    pub schedule_id: Option<ProtoScheduleId>,
    /// Replacement serde-encoded `aion_core::ScheduleConfig` envelope.
    #[prost(message, optional, tag = "3")]
    pub config: Option<WireEnvelope>,
}

/// Proto representation of `UpdateScheduleResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoUpdateScheduleResponse {
    /// Serde-encoded `aion::schedule::ScheduleState` envelope after update.
    #[prost(message, optional, tag = "1")]
    pub state: Option<WireEnvelope>,
}

/// Proto representation of a schedule-id request.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoScheduleIdRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Target schedule identifier.
    #[prost(message, optional, tag = "2")]
    pub schedule_id: Option<ProtoScheduleId>,
}

/// Proto representation of `PauseScheduleResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoPauseScheduleResponse {
    /// Serde-encoded `aion::schedule::ScheduleState` envelope after pause.
    #[prost(message, optional, tag = "1")]
    pub state: Option<WireEnvelope>,
}

/// Proto representation of `ResumeScheduleResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoResumeScheduleResponse {
    /// Serde-encoded `aion::schedule::ScheduleState` envelope after resume.
    #[prost(message, optional, tag = "1")]
    pub state: Option<WireEnvelope>,
}

/// Proto representation of `DeleteScheduleResponse`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoDeleteScheduleResponse {}

/// Proto representation of `ListSchedulesRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoListSchedulesRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
}

/// Proto representation of `ListSchedulesResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoListSchedulesResponse {
    /// Serde-encoded `aion::schedule::ScheduleState` envelopes.
    #[prost(message, repeated, tag = "1")]
    pub schedules: Vec<WireEnvelope>,
}

/// Proto representation of `DescribeScheduleResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoDescribeScheduleResponse {
    /// Serde-encoded `aion::schedule::ScheduleState` envelope.
    #[prost(message, optional, tag = "1")]
    pub state: Option<WireEnvelope>,
}
