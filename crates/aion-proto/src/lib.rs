//! Shared gRPC and serde wire contracts for Aion servers, clients, and workers.
//!
//! This crate mirrors core domain values into transport-safe protobuf and JSON
//! shapes, provides conversion helpers, and optionally exposes generated tonic
//! service definitions behind the `generated` feature.
//!
//! # Example
//!
//! ```
//! use aion_core::WorkflowId;
//! use aion_proto::{decode_core_value, encode_core_value};
//!
//! let id = WorkflowId::new_v4();
//! let envelope = encode_core_value("default", Some("request-1".to_owned()), &id)?;
//! let decoded: WorkflowId = decode_core_value(&envelope)?;
//! assert_eq!(decoded, id);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

/// Conversion helpers between wire structures and `aion-core` values.
pub mod convert;
/// Operator deploy API wire contracts.
pub mod deploy;
/// Wire-level error types and protobuf-friendly error payloads.
pub mod error;
/// Event-stream subscription and streamed-event contracts.
pub mod events;
/// Schedule management wire contracts.
pub mod schedule;
/// Remote-worker protocol wire contracts.
pub mod worker;
/// Workflow operation wire contracts.
pub mod workflow;

#[cfg(feature = "generated")]
/// Generated tonic service definitions compiled from Aion protobuf files.
pub mod generated;

pub use convert::{
    ProtoActivityId, ProtoPayload, ProtoRunId, ProtoScheduleId, ProtoTimerId, ProtoWorkflowId,
    ProtoWorkflowStatus, WireEnvelope, decode_core_value, decode_event, decode_schedule_config,
    decode_schedule_state, decode_workflow_filter, decode_workflow_summary, encode_core_value,
    encode_event, encode_schedule_config, encode_schedule_state, encode_workflow_filter,
    encode_workflow_summary,
};
pub use deploy::{
    ProtoListVersionsRequest, ProtoListVersionsResponse, ProtoLoadPackageRequest,
    ProtoLoadPackageResponse, ProtoRouteVersionRequest, ProtoRouteVersionResponse,
    ProtoUnloadVersionRequest, ProtoUnloadVersionResponse, ProtoWorkflowVersion,
};
pub use error::{ProtoWireError, ProtoWireErrorCode, WireError, WireErrorCode};
pub use events::{
    FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, StreamedEvent,
    SubscriptionRequest, encode_streamed_event, subscription_request,
};
pub use schedule::{
    ProtoCreateScheduleRequest, ProtoCreateScheduleResponse, ProtoDeleteScheduleResponse,
    ProtoDescribeScheduleResponse, ProtoListSchedulesRequest, ProtoListSchedulesResponse,
    ProtoPauseScheduleResponse, ProtoResumeScheduleResponse, ProtoScheduleIdRequest,
    ProtoUpdateScheduleRequest, ProtoUpdateScheduleResponse,
};
pub use worker::{
    ProtoActivityError, ProtoActivityErrorKind, ProtoActivityResult, ProtoActivityTask,
    ProtoDrainRequest, ProtoHeartbeat, ProtoRegisterAck, ProtoRegisterWorker, ProtoResultAck,
    proto_activity_result,
};
pub use workflow::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoCountWorkflowsResponse, ProtoDescribeWorkflowRequest, ProtoDescribeWorkflowResponse,
    ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoQueryRequest, ProtoQueryResponse,
    ProtoSignalRequest, ProtoSignalResponse, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse,
    proto_query_response,
};
