//! The shared wire contract: gRPC service definitions and serde wire types used by the server, all client SDKs, and all worker SDKs. Depends only on aion-core for type parity.

pub mod convert;
pub mod error;
pub mod worker;

pub use convert::{
    ProtoActivityId, ProtoPayload, ProtoRunId, ProtoTimerId, ProtoWorkflowId, ProtoWorkflowStatus,
    WireEnvelope, decode_core_value, encode_core_value,
};
pub use error::{ProtoWireError, ProtoWireErrorCode, WireError, WireErrorCode};
pub use worker::{
    ProtoActivityError, ProtoActivityErrorKind, ProtoActivityResult, ProtoActivityTask,
    ProtoHeartbeat, ProtoRegisterWorker, proto_activity_result,
};
