//! The shared wire contract: gRPC service definitions and serde wire types used by the server, all client SDKs, and all worker SDKs. Depends only on aion-core for type parity.

pub mod convert;
pub mod error;
pub mod events;
pub mod worker;
pub mod workflow;

#[cfg(feature = "generated")]
pub mod generated;

pub use convert::{
    ProtoActivityId, ProtoPayload, ProtoRunId, ProtoTimerId, ProtoWorkflowId, ProtoWorkflowStatus,
    WireEnvelope, decode_core_value, decode_event, decode_workflow_filter, decode_workflow_summary,
    encode_core_value, encode_event, encode_workflow_filter, encode_workflow_summary,
};
pub use error::{ProtoWireError, ProtoWireErrorCode, WireError, WireErrorCode};
pub use events::{
    FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, StreamedEvent,
    SubscriptionRequest, encode_streamed_event, subscription_request,
};
pub use worker::{
    ProtoActivityError, ProtoActivityErrorKind, ProtoActivityResult, ProtoActivityTask,
    ProtoHeartbeat, ProtoRegisterWorker, proto_activity_result,
};
pub use workflow::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoCountWorkflowsResponse, ProtoDescribeWorkflowRequest, ProtoDescribeWorkflowResponse,
    ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoQueryRequest, ProtoQueryResponse,
    ProtoSignalRequest, ProtoSignalResponse, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse,
    proto_query_response,
};
