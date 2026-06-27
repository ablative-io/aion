//! Workflow-management serde/prost wire types.

use crate::convert::{ProtoPayload, ProtoRunId, ProtoWorkflowId, WireEnvelope};
use crate::error::ProtoWireError;

/// Proto representation of `StartWorkflowRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoStartWorkflowRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Workflow type name registered with the engine.
    #[prost(string, tag = "2")]
    pub workflow_type: String,
    /// Workflow start input payload.
    #[prost(message, optional, tag = "3")]
    pub input: Option<ProtoPayload>,
    /// R-4 steered-start routing key. When set, the start is steered to
    /// `shard_for(routing_key)`'s owner (forwarded there when this node is not the
    /// owner). `None`/empty keeps the unsteered R-1 remint behaviour.
    #[prost(string, optional, tag = "4")]
    pub routing_key: Option<String>,
}

/// Proto representation of `StartWorkflowResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoStartWorkflowResponse {
    /// Assigned workflow identifier.
    #[prost(message, optional, tag = "1")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Assigned concrete run identifier.
    #[prost(message, optional, tag = "2")]
    pub run_id: Option<ProtoRunId>,
}

/// Proto representation of `SignalRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoSignalRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Target workflow identifier.
    #[prost(message, optional, tag = "2")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Target run identifier.
    #[prost(message, optional, tag = "3")]
    pub run_id: Option<ProtoRunId>,
    /// Signal name registered by workflow code.
    #[prost(string, tag = "4")]
    pub signal_name: String,
    /// Signal payload.
    #[prost(message, optional, tag = "5")]
    pub payload: Option<ProtoPayload>,
}

/// Proto representation of `SignalResponse`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoSignalResponse {}

/// Proto representation of `QueryRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoQueryRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Target workflow identifier.
    #[prost(message, optional, tag = "2")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Target run identifier.
    #[prost(message, optional, tag = "3")]
    pub run_id: Option<ProtoRunId>,
    /// Query name registered by workflow code.
    #[prost(string, tag = "4")]
    pub query_name: String,
}

/// Proto representation of `QueryResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoQueryResponse {
    /// Query result or typed wire error.
    #[prost(oneof = "proto_query_response::Outcome", tags = "1, 2")]
    pub outcome: Option<proto_query_response::Outcome>,
}

/// Types nested under [`ProtoQueryResponse`].
pub mod proto_query_response {
    /// Proto oneof for successful query payloads and typed failures.
    #[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Oneof)]
    pub enum Outcome {
        /// Query result payload.
        #[prost(message, tag = "1")]
        Result(super::ProtoPayload),
        /// Typed query error.
        #[prost(message, tag = "2")]
        Error(super::ProtoWireError),
    }
}

/// Proto representation of `CancelRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoCancelRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Target workflow identifier.
    #[prost(message, optional, tag = "2")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Target run identifier.
    #[prost(message, optional, tag = "3")]
    pub run_id: Option<ProtoRunId>,
    /// Human-readable cancellation reason.
    #[prost(string, tag = "4")]
    pub reason: String,
}

/// Proto representation of `CancelResponse`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoCancelResponse {}

/// Proto representation of `ListWorkflowsRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoListWorkflowsRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Serde-encoded `aion_store::visibility::ListWorkflowsFilter` envelope.
    #[prost(message, optional, tag = "2")]
    pub filter: Option<WireEnvelope>,
}

/// Proto representation of `ListWorkflowsResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoListWorkflowsResponse {
    /// Serde-encoded `aion_store::visibility::WorkflowSummary` envelopes.
    #[prost(message, repeated, tag = "1")]
    pub summaries: Vec<WireEnvelope>,
}

/// Proto representation of `CountWorkflowsRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoCountWorkflowsRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Serde-encoded `aion_store::visibility::ListWorkflowsFilter` envelope.
    #[prost(message, optional, tag = "2")]
    pub filter: Option<WireEnvelope>,
}

/// Proto representation of `CountWorkflowsResponse`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoCountWorkflowsResponse {
    /// Number of visibility summaries matching the filter.
    #[prost(uint64, tag = "1")]
    pub count: u64,
}

/// Proto representation of `DescribeWorkflowRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoDescribeWorkflowRequest {
    /// Namespace that scopes the operation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Target workflow identifier.
    #[prost(message, optional, tag = "2")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// Target run identifier.
    #[prost(message, optional, tag = "3")]
    pub run_id: Option<ProtoRunId>,
    /// Whether event history should be included in the response.
    #[prost(bool, tag = "4")]
    pub include_history: bool,
}

/// Proto representation of `DescribeWorkflowResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoDescribeWorkflowResponse {
    /// Serde-encoded `aion_core::WorkflowSummary` envelope.
    #[prost(message, optional, tag = "1")]
    pub summary: Option<WireEnvelope>,
    /// Optional serde-encoded `aion_core::Event` envelopes.
    #[prost(message, repeated, tag = "2")]
    pub history: Vec<WireEnvelope>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use aion_core::SearchAttributeValue;
    use aion_store::visibility::{ListWorkflowsFilter, SearchAttributePredicate};
    use chrono::{DateTime, Utc};
    use prost::Message;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::{
        ProtoCountWorkflowsRequest, ProtoCountWorkflowsResponse, ProtoListWorkflowsRequest,
        ProtoListWorkflowsResponse, ProtoQueryRequest, ProtoQueryResponse,
        ProtoStartWorkflowRequest, ProtoStartWorkflowResponse, proto_query_response,
    };
    use crate::convert::{
        ProtoPayload, ProtoRunId, ProtoWorkflowId, decode_core_value, encode_core_value,
    };
    use crate::error::{ProtoWireError, WireError};

    fn workflow_id() -> aion_core::WorkflowId {
        aion_core::WorkflowId::new(uuid::Uuid::nil())
    }

    fn run_id() -> aion_core::RunId {
        aion_core::RunId::new(uuid::Uuid::nil())
    }

    fn payload(label: &str) -> Result<ProtoPayload, aion_core::PayloadError> {
        Ok(ProtoPayload::from(aion_core::Payload::from_json(
            &json!({ "label": label }),
        )?))
    }

    fn recorded_at() -> Result<DateTime<Utc>, chrono::ParseError> {
        Ok(DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")?.with_timezone(&Utc))
    }

    fn assert_json_round_trip<T>(value: &T) -> Result<(), serde_json::Error>
    where
        T: Clone + PartialEq + serde::Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_string(value)?;
        let decoded = serde_json::from_str::<T>(&encoded)?;
        assert!(decoded == *value);
        Ok(())
    }

    fn assert_proto_round_trip<T>(value: &T) -> Result<(), Box<dyn std::error::Error>>
    where
        T: Clone + PartialEq + Message + Default,
    {
        let mut bytes = Vec::new();
        value.encode(&mut bytes)?;
        let decoded = T::decode(bytes.as_slice())?;
        assert!(decoded == *value);
        Ok(())
    }

    #[test]
    fn start_workflow_round_trips_json_and_proto() -> Result<(), Box<dyn std::error::Error>> {
        let request = ProtoStartWorkflowRequest {
            namespace: String::from("tenant-a"),
            workflow_type: String::from("checkout"),
            input: Some(payload("input")?),
            routing_key: Some(String::from("tenant-a/order-1")),
        };
        let response = ProtoStartWorkflowResponse {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            run_id: Some(ProtoRunId::from(run_id())),
        };

        assert_json_round_trip(&request)?;
        assert_proto_round_trip(&request)?;
        assert_json_round_trip(&response)?;
        assert_proto_round_trip(&response)?;
        Ok(())
    }

    #[test]
    fn list_workflows_round_trips_json_and_proto() -> Result<(), Box<dyn std::error::Error>> {
        let filter = ListWorkflowsFilter {
            workflow_type: Some(String::from("checkout")),
            status: Some(aion_core::WorkflowStatus::Running),
            search_attributes: vec![SearchAttributePredicate::Equals {
                name: String::from("customer_id"),
                value: SearchAttributeValue::String(String::from("12345")),
            }],
            limit: Some(10),
            offset: Some(5),
            ..ListWorkflowsFilter::default()
        };
        let summary = aion_store::visibility::WorkflowSummary {
            workflow_id: workflow_id(),
            run_id: run_id(),
            workflow_type: String::from("checkout"),
            status: aion_core::WorkflowStatus::Running,
            start_time: recorded_at()?,
            close_time: None,
            search_attributes: HashMap::from([(
                String::from("customer_id"),
                SearchAttributeValue::String(String::from("12345")),
            )]),
        };
        let filter_envelope = encode_core_value("tenant-a", Some(String::from("r1")), &filter)?;
        let summary_envelope = encode_core_value("tenant-a", None, &summary)?;
        let request = ProtoListWorkflowsRequest {
            namespace: String::from("tenant-a"),
            filter: Some(filter_envelope.clone()),
        };
        let response = ProtoListWorkflowsResponse {
            summaries: vec![summary_envelope.clone()],
        };
        let count_request = ProtoCountWorkflowsRequest {
            namespace: String::from("tenant-a"),
            filter: Some(filter_envelope.clone()),
        };
        let count_response = ProtoCountWorkflowsResponse { count: 1 };

        assert_json_round_trip(&request)?;
        assert_proto_round_trip(&request)?;
        assert_json_round_trip(&response)?;
        assert_proto_round_trip(&response)?;
        assert_json_round_trip(&count_request)?;
        assert_proto_round_trip(&count_request)?;
        assert_json_round_trip(&count_response)?;
        assert_proto_round_trip(&count_response)?;
        assert_eq!(
            decode_core_value::<ListWorkflowsFilter>(&filter_envelope)?,
            filter
        );
        assert_eq!(
            decode_core_value::<aion_store::visibility::WorkflowSummary>(&summary_envelope)?,
            summary
        );
        Ok(())
    }

    #[test]
    fn query_round_trips_json_and_proto() -> Result<(), Box<dyn std::error::Error>> {
        let request = ProtoQueryRequest {
            namespace: String::from("tenant-a"),
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            run_id: Some(ProtoRunId::from(run_id())),
            query_name: String::from("state"),
        };
        let result_response = ProtoQueryResponse {
            outcome: Some(proto_query_response::Outcome::Result(payload("result")?)),
        };
        let error_response = ProtoQueryResponse {
            outcome: Some(proto_query_response::Outcome::Error(ProtoWireError::from(
                WireError::unknown_query("state query is not registered"),
            ))),
        };

        assert_json_round_trip(&request)?;
        assert_proto_round_trip(&request)?;
        assert_json_round_trip(&result_response)?;
        assert_proto_round_trip(&result_response)?;
        assert_json_round_trip(&error_response)?;
        assert_proto_round_trip(&error_response)?;
        Ok(())
    }
}
