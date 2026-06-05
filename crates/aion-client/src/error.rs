//! `ClientError` taxonomy and transport/proto error mapping.

use aion_proto::{ProtoWireError, WireError, WireErrorCode};
use prost::Message;
use tonic::Code;

/// Branchable caller-side error taxonomy shared by every aion client SDK.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The requested workflow or run does not exist.
    #[error("not found")]
    NotFound,
    /// A caller-supplied idempotency key conflicts with a different request.
    #[error("already exists")]
    AlreadyExists,
    /// The workflow query handler failed.
    #[error("query failed")]
    QueryFailed,
    /// The workflow query exceeded its deadline.
    #[error("query timed out")]
    QueryTimeout,
    /// The call or target workflow was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The server or network transport is unavailable.
    #[error("unavailable")]
    Unavailable,
    /// Authentication credentials were rejected.
    #[error("unauthenticated")]
    Unauthenticated,
    /// The request was malformed or targets an unsupported operation state.
    #[error("invalid argument")]
    InvalidArgument,
    /// The server reported an unexpected internal failure.
    #[error("server error: {detail}")]
    Server {
        /// Informational server detail.
        detail: String,
    },
}

impl ClientError {
    /// Converts an AW wire error into the client SDK taxonomy.
    #[must_use]
    pub fn from_wire_error(error: WireError) -> Self {
        map_wire_parts(error.code, error.message)
    }

    /// Converts a proto-encoded wire error into the client SDK taxonomy.
    #[must_use]
    pub fn from_proto_wire_error(error: ProtoWireError) -> Self {
        match WireError::try_from(error) {
            Ok(error) | Err(error) => Self::from_wire_error(error),
        }
    }

    /// Converts a tonic status into the client SDK taxonomy.
    #[must_use]
    pub fn from_status(status: &tonic::Status) -> Self {
        if let Some(error) = decode_status_details(status) {
            return Self::from_proto_wire_error(error);
        }

        match status.code() {
            Code::NotFound => Self::NotFound,
            Code::AlreadyExists | Code::Aborted => Self::AlreadyExists,
            Code::DeadlineExceeded => Self::QueryTimeout,
            Code::Cancelled => Self::Cancelled,
            Code::Unavailable | Code::ResourceExhausted => Self::Unavailable,
            Code::Unauthenticated => Self::Unauthenticated,
            Code::InvalidArgument | Code::FailedPrecondition | Code::PermissionDenied => {
                Self::InvalidArgument
            }
            _ => Self::Server {
                detail: status.message().to_owned(),
            },
        }
    }

    /// Converts a tonic transport failure into the client SDK taxonomy.
    #[must_use]
    pub fn from_transport_error(_: tonic::transport::Error) -> Self {
        Self::Unavailable
    }

    /// Converts a local conversion/detail failure into an unexpected server error.
    #[must_use]
    pub fn server(detail: impl Into<String>) -> Self {
        Self::Server {
            detail: detail.into(),
        }
    }
}

fn decode_status_details(status: &tonic::Status) -> Option<ProtoWireError> {
    let details = status.details();
    if details.is_empty() {
        return None;
    }
    ProtoWireError::decode(details).ok()
}

fn map_wire_parts(code: WireErrorCode, detail: String) -> ClientError {
    match code {
        WireErrorCode::NotFound => ClientError::NotFound,
        WireErrorCode::NamespaceDenied
        | WireErrorCode::UnknownQuery
        | WireErrorCode::NotRunning => ClientError::InvalidArgument,
        WireErrorCode::SequenceConflict => ClientError::AlreadyExists,
        WireErrorCode::QueryTimeout => ClientError::QueryTimeout,
        WireErrorCode::Lagged => ClientError::Unavailable,
        WireErrorCode::Backend => ClientError::Server { detail },
    }
}

#[cfg(test)]
mod tests {
    use super::ClientError;
    use aion_proto::WireError;
    use prost::Message;
    use tonic::{Code, Status};

    fn assert_send_sync_static<T: Send + Sync + 'static>() {}

    #[test]
    fn client_error_is_send_sync_static() {
        assert_send_sync_static::<ClientError>();
    }

    #[test]
    fn maps_known_wire_errors_without_collapsing_to_server() {
        assert_eq!(
            ClientError::from_wire_error(WireError::not_found("missing")),
            ClientError::NotFound
        );
        assert_eq!(
            ClientError::from_wire_error(WireError::sequence_conflict("conflict")),
            ClientError::AlreadyExists
        );
        assert_eq!(
            ClientError::from_wire_error(WireError::query_timeout("slow")),
            ClientError::QueryTimeout
        );
        assert_eq!(
            ClientError::from_wire_error(WireError::lagged("behind")),
            ClientError::Unavailable
        );
    }

    #[test]
    fn decodes_proto_wire_error_from_tonic_status_details() {
        let proto = aion_proto::ProtoWireError::from(WireError::not_found("missing"));
        let mut details = Vec::new();
        let encode_result = proto.encode(&mut details);
        assert!(encode_result.is_ok());
        let status = Status::with_details(Code::NotFound, "missing", details.into());

        assert_eq!(ClientError::from_status(&status), ClientError::NotFound);
    }

    #[test]
    fn maps_tonic_status_fallbacks() {
        let unavailable = Status::new(Code::Unavailable, "down");
        let unauthenticated = Status::new(Code::Unauthenticated, "bad token");
        let deadline = Status::new(Code::DeadlineExceeded, "slow");

        assert_eq!(
            ClientError::from_status(&unavailable),
            ClientError::Unavailable
        );
        assert_eq!(
            ClientError::from_status(&unauthenticated),
            ClientError::Unauthenticated
        );
        assert_eq!(
            ClientError::from_status(&deadline),
            ClientError::QueryTimeout
        );
    }
}
