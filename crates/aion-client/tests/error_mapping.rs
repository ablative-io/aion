//! Wire/status -> `ClientError` mapping contract: every server-supplied
//! message and `error_type` must survive into the carried `ErrorDetail`, and
//! the four query wire codes must stay distinct variants.

use aion_client::ClientError;
use aion_client::error::ErrorDetail;
use aion_proto::{ProtoWireError, WireError};
use prost::Message;
use tonic::{Code, Status};

#[test]
fn wire_errors_map_to_distinct_variants_preserving_detail() {
    assert_eq!(
        ClientError::from_wire_error(WireError::not_found("workflow missing")),
        ClientError::not_found("workflow missing")
    );
    assert_eq!(
        ClientError::from_wire_error(WireError::query_failed("handler raised")),
        ClientError::query_failed("handler raised")
    );
    assert_eq!(
        ClientError::from_wire_error(WireError::query_timeout("query window elapsed")),
        ClientError::query_timeout("query window elapsed")
    );
    assert_eq!(
        ClientError::from_wire_error(WireError::unknown_query("no query named 'stat'")),
        ClientError::unknown_query("no query named 'stat'")
    );
    assert_eq!(
        ClientError::from_wire_error(WireError::not_running("run reached Completed")),
        ClientError::not_running("run reached Completed")
    );
    assert_eq!(
        ClientError::from_wire_error(WireError::lagged("subscriber fell behind")),
        ClientError::unavailable("subscriber fell behind")
    );
    assert_eq!(
        ClientError::from_wire_error(WireError::invalid_input("resume_from_seq must be >= 1")),
        ClientError::invalid_argument("resume_from_seq must be >= 1")
    );
    assert_eq!(
        ClientError::from_wire_error(WireError::namespace_denied(
            "namespace tenant-b is not granted to this caller"
        )),
        ClientError::namespace_denied("namespace tenant-b is not granted to this caller")
    );
}

#[test]
fn wire_error_type_discriminator_is_preserved() {
    let error = ClientError::from_wire_error(WireError::not_found_with_type(
        "WorkflowNotFound",
        "workflow was not found",
    ));
    assert_eq!(
        error,
        ClientError::not_found(ErrorDetail::with_type(
            "workflow was not found",
            "WorkflowNotFound"
        ))
    );
    assert_eq!(
        error.detail().error_type.as_deref(),
        Some("WorkflowNotFound")
    );

    let backend = ClientError::from_wire_error(WireError::backend_with_type(
        "Durability",
        "store unavailable",
    ));
    assert_eq!(
        backend,
        ClientError::server(ErrorDetail::with_type("store unavailable", "Durability"))
    );
}

#[test]
fn sequence_conflict_is_a_server_bug_not_already_exists() {
    // The server has no idempotency-key feature; sequence_conflict is its
    // internal double-writer invariant violation.
    assert_eq!(
        ClientError::from_wire_error(WireError::sequence_conflict("conflict")),
        ClientError::server("conflict")
    );

    let aborted = Status::new(Code::Aborted, "sequence position conflicted");
    assert_eq!(
        ClientError::from_status(&aborted),
        ClientError::server("sequence position conflicted")
    );

    let already_exists = Status::new(Code::AlreadyExists, "duplicate");
    assert_eq!(
        ClientError::from_status(&already_exists),
        ClientError::already_exists("duplicate")
    );
}

#[test]
fn status_details_carry_the_authoritative_typed_wire_error() -> Result<(), prost::EncodeError> {
    let proto = ProtoWireError::from(WireError::not_running_with_type(
        "ShuttingDown",
        "engine is shutting down",
    ));
    let mut details = Vec::new();
    proto.encode(&mut details)?;
    let status = Status::with_details(Code::FailedPrecondition, "shutting down", details.into());

    assert_eq!(
        ClientError::from_status(&status),
        ClientError::not_running(ErrorDetail::with_type(
            "engine is shutting down",
            "ShuttingDown"
        ))
    );
    Ok(())
}

#[test]
fn status_fallback_without_details_keeps_the_status_message() {
    let cases = [
        (Code::NotFound, ClientError::not_found("m")),
        (Code::AlreadyExists, ClientError::already_exists("m")),
        (Code::DeadlineExceeded, ClientError::query_timeout("m")),
        (Code::Cancelled, ClientError::cancelled("m")),
        (Code::Unavailable, ClientError::unavailable("m")),
        (Code::ResourceExhausted, ClientError::unavailable("m")),
        (Code::Unauthenticated, ClientError::unauthenticated("m")),
        (Code::PermissionDenied, ClientError::namespace_denied("m")),
        (Code::InvalidArgument, ClientError::invalid_argument("m")),
        // The server emits FAILED_PRECONDITION only for `not_running`.
        (Code::FailedPrecondition, ClientError::not_running("m")),
        (Code::Internal, ClientError::server("m")),
    ];
    for (code, expected) in cases {
        assert_eq!(
            ClientError::from_status(&Status::new(code, "m")),
            expected,
            "gRPC {code:?} fallback mapping"
        );
    }
}

#[test]
fn namespace_denied_proto_details_override_the_status_message() -> Result<(), prost::EncodeError> {
    let proto = ProtoWireError::from(WireError::namespace_denied("tenant-b not visible"));
    let mut details = Vec::new();
    proto.encode(&mut details)?;
    let status = Status::with_details(Code::PermissionDenied, "denied", details.into());

    assert_eq!(
        ClientError::from_status(&status),
        ClientError::namespace_denied("tenant-b not visible")
    );
    Ok(())
}
