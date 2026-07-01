//! Wire-error-to-tonic-Status mapping for the workflow gRPC service.
//!
//! Peeled out of `grpc/mod.rs` (AO-007 500-code-line split). The two
//! `pub(crate)` builders are re-exported by `mod.rs` so `deploy_grpc` keeps its
//! `crate::api::grpc::{status_from_wire_error, status_with_code}` paths.

use aion_proto::{ProtoWireError, WireError};
use prost::Message;
use tonic::{Code, Status};

pub(crate) fn status_from_wire_error(error: WireError) -> Status {
    status_with_code(grpc_code(error.code), error)
}

/// Build a tonic status with an explicit code, carrying the typed
/// `ProtoWireError` detail payload when it encodes.
pub(crate) fn status_with_code(code: Code, error: WireError) -> Status {
    let message = error.message.clone();
    let mut details = Vec::new();
    let proto_error = ProtoWireError::from(error);
    if proto_error.encode(&mut details).is_ok() {
        Status::with_details(code, message, details.into())
    } else {
        Status::new(code, message)
    }
}

fn grpc_code(code: aion_proto::WireErrorCode) -> Code {
    match code {
        aion_proto::WireErrorCode::NotFound => Code::NotFound,
        aion_proto::WireErrorCode::NamespaceDenied | aion_proto::WireErrorCode::DeployDenied => {
            Code::PermissionDenied
        }
        // Wrong-shard-owner (fenced) is a retryable routing signal: surface it as
        // `Aborted`, the same retryable code the CAS `SequenceConflict` precedent
        // uses (R-0). A routing-aware caller re-resolves the owner and retries.
        aion_proto::WireErrorCode::SequenceConflict | aion_proto::WireErrorCode::NotOwner => {
            Code::Aborted
        }
        aion_proto::WireErrorCode::UnknownQuery | aion_proto::WireErrorCode::InvalidInput => {
            Code::InvalidArgument
        }
        aion_proto::WireErrorCode::QueryTimeout => Code::DeadlineExceeded,
        aion_proto::WireErrorCode::NotRunning
        | aion_proto::WireErrorCode::VersionPinned
        | aion_proto::WireErrorCode::InvalidState => Code::FailedPrecondition,
        aion_proto::WireErrorCode::Lagged => Code::ResourceExhausted,
        // query_failed normally rides QueryResponse.error inside an OK
        // response; a transport-level carrier still attaches the typed
        // ProtoWireError detail, so detail-aware clients keep QueryFailed.
        aion_proto::WireErrorCode::Backend | aion_proto::WireErrorCode::QueryFailed => {
            Code::Internal
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_proto::WireErrorCode;
    use tonic::Code;

    use super::grpc_code;

    /// R-0: the typed wrong-shard-owner fence maps to the retryable `Aborted`
    /// gRPC code (the same code the CAS `SequenceConflict` precedent uses), not
    /// the opaque `Internal` the stringly-typed fence used to collapse into.
    #[test]
    fn not_owner_wire_code_maps_to_retryable_aborted() {
        assert_eq!(grpc_code(WireErrorCode::NotOwner), Code::Aborted);
    }

    /// The reopen precondition (`invalid_state`) maps to `FailedPrecondition`
    /// (AO-007 C35), the same retryable-not gRPC class as `not_running`.
    #[test]
    fn invalid_state_wire_code_maps_to_failed_precondition() {
        assert_eq!(
            grpc_code(WireErrorCode::InvalidState),
            Code::FailedPrecondition
        );
    }
}
