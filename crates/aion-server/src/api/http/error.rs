//! Wire-error-to-HTTP response mapping.

use aion_proto::{WireError, WireErrorCode};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub(crate) struct HttpWireError(pub(crate) WireError);

impl IntoResponse for HttpWireError {
    fn into_response(self) -> Response {
        (http_status(self.0.code), Json(self.0)).into_response()
    }
}

fn http_status(code: WireErrorCode) -> StatusCode {
    match code {
        WireErrorCode::NotFound => StatusCode::NOT_FOUND,
        WireErrorCode::NamespaceDenied | WireErrorCode::DeployDenied => StatusCode::FORBIDDEN,
        // NotOwner (wrong-shard-owner fence) is retryable, like the CAS
        // SequenceConflict precedent: a 409 the caller re-resolves and retries.
        // InvalidState (e.g. a non-reopenable-terminal reopen) is a state
        // conflict on the target: HTTP 409 Conflict, per AD-012.
        WireErrorCode::SequenceConflict
        | WireErrorCode::VersionPinned
        | WireErrorCode::NotOwner
        | WireErrorCode::InvalidState => StatusCode::CONFLICT,
        WireErrorCode::UnknownQuery | WireErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        WireErrorCode::QueryTimeout => StatusCode::REQUEST_TIMEOUT,
        WireErrorCode::NotRunning => StatusCode::PRECONDITION_FAILED,
        WireErrorCode::Lagged => StatusCode::TOO_MANY_REQUESTS,
        // query_failed normally rides QueryResponse.error inside a 200; when
        // a transport-level surface must carry it, it is a server-reported
        // application-level handler failure alongside backend faults.
        WireErrorCode::Backend | WireErrorCode::QueryFailed => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(crate) enum HttpStartError {
    Wire(HttpWireError),
    Draining,
}

impl IntoResponse for HttpStartError {
    fn into_response(self) -> Response {
        match self {
            Self::Wire(error) => error.into_response(),
            Self::Draining => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(WireError::backend(
                    "server is draining and not accepting new workflow starts",
                )),
            )
                .into_response(),
        }
    }
}
