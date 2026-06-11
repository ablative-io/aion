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
    /// The caller's credential was accepted, but the caller has no grant for
    /// the requested namespace.
    ///
    /// This is exactly a namespace-grant failure. Workflow-level invisibility
    /// — the workflow does not exist, or is owned by another namespace — is
    /// reported as [`ClientError::NotFound`] so a cross-tenant probe is
    /// indistinguishable from a nonexistent workflow.
    ///
    /// Maps from the AW wire error code `namespace_denied` and gRPC
    /// `PERMISSION_DENIED`. Distinct from [`ClientError::Unauthenticated`]
    /// (credential rejected or unvalidatable) and from
    /// [`ClientError::InvalidArgument`] (malformed or invalid request). Not
    /// retryable until the caller's grants change.
    #[error("namespace denied: {detail}")]
    NamespaceDenied {
        /// Server-supplied denial detail message.
        detail: String,
    },
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
            Code::AlreadyExists => Self::AlreadyExists,
            Code::DeadlineExceeded => Self::QueryTimeout,
            Code::Cancelled => Self::Cancelled,
            Code::Unavailable | Code::ResourceExhausted => Self::Unavailable,
            Code::Unauthenticated => Self::Unauthenticated,
            Code::PermissionDenied => Self::NamespaceDenied {
                detail: status.message().to_owned(),
            },
            Code::InvalidArgument | Code::FailedPrecondition => Self::InvalidArgument,
            // ABORTED deliberately falls through to Server: the server sends
            // it only for `sequence_conflict`, an internal single-writer
            // invariant violation (a double-writer bug), never an
            // idempotency conflict — so it must not map to AlreadyExists.
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
        WireErrorCode::NamespaceDenied => ClientError::NamespaceDenied { detail },
        WireErrorCode::UnknownQuery | WireErrorCode::NotRunning | WireErrorCode::InvalidInput => {
            ClientError::InvalidArgument
        }
        // `sequence_conflict` is emitted solely for the server's internal
        // single-writer invariant violation (a double-writer bug). The server
        // has no idempotency-key feature, so this is never AlreadyExists; it
        // is an unexpected server failure.
        WireErrorCode::SequenceConflict | WireErrorCode::Backend => ClientError::Server { detail },
        WireErrorCode::QueryFailed => ClientError::QueryFailed,
        WireErrorCode::QueryTimeout => ClientError::QueryTimeout,
        WireErrorCode::Lagged => ClientError::Unavailable,
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
            ClientError::from_wire_error(WireError::query_timeout("slow")),
            ClientError::QueryTimeout
        );
        assert_eq!(
            ClientError::from_wire_error(WireError::query_failed("handler raised")),
            ClientError::QueryFailed
        );
        assert_eq!(
            ClientError::from_wire_error(WireError::lagged("behind")),
            ClientError::Unavailable
        );
    }

    #[test]
    fn sequence_conflict_is_a_server_bug_not_already_exists() {
        // The server has no idempotency-key feature; sequence_conflict is its
        // internal double-writer invariant violation.
        assert_eq!(
            ClientError::from_wire_error(WireError::sequence_conflict("conflict")),
            ClientError::Server {
                detail: String::from("conflict"),
            }
        );

        let aborted = Status::new(Code::Aborted, "sequence position conflicted");
        assert_eq!(
            ClientError::from_status(&aborted),
            ClientError::Server {
                detail: String::from("sequence position conflicted"),
            }
        );

        let already_exists = Status::new(Code::AlreadyExists, "duplicate");
        assert_eq!(
            ClientError::from_status(&already_exists),
            ClientError::AlreadyExists
        );
    }

    #[test]
    fn maps_namespace_denied_wire_error_preserving_detail() {
        assert_eq!(
            ClientError::from_wire_error(WireError::namespace_denied(
                "namespace tenant-b is not granted to this caller"
            )),
            ClientError::NamespaceDenied {
                detail: String::from("namespace tenant-b is not granted to this caller"),
            }
        );
    }

    #[test]
    fn permission_denied_status_without_details_falls_back_to_namespace_denied() {
        let status = Status::new(Code::PermissionDenied, "namespace tenant-b denied");

        assert_eq!(
            ClientError::from_status(&status),
            ClientError::NamespaceDenied {
                detail: String::from("namespace tenant-b denied"),
            }
        );
    }

    #[test]
    fn decodes_namespace_denied_proto_wire_error_from_tonic_status_details() {
        let proto =
            aion_proto::ProtoWireError::from(WireError::namespace_denied("tenant-b not visible"));
        let mut details = Vec::new();
        let encode_result = proto.encode(&mut details);
        assert!(encode_result.is_ok());
        let status = Status::with_details(Code::PermissionDenied, "denied", details.into());

        assert_eq!(
            ClientError::from_status(&status),
            ClientError::NamespaceDenied {
                detail: String::from("tenant-b not visible"),
            }
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
