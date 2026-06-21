//! `/authoring/*` HTTP facade over the shared authoring handlers.
//!
//! Mounted only when `[authoring].gleam_path` is configured; the absent
//! surface is a plain 404 (see `router.rs`). `POST /authoring/compile` takes a
//! JSON `{ "source": "..." }` body, compiles and type-checks it through the
//! external `gleam` binary, and on success hot-loads the package, returning
//! the workflow type and content hash. A type error is returned inline as a
//! 400 carrying the verbatim compiler diagnostics.

use aion_proto::WireError;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::ServerState;
use crate::authoring::{self, AuthoringApiError, CompileSourceRequest, CompileSourceResponse};

const TRANSPORT: &str = "http";

/// Authoring failure response: 400 for a type error (carrying the verbatim
/// gleam diagnostics inline), 503 for drain/shutdown, and the standard
/// wire-code table otherwise.
pub(crate) struct AuthoringHttpError(pub(crate) AuthoringApiError);

impl IntoResponse for AuthoringHttpError {
    fn into_response(self) -> Response {
        match self.0 {
            AuthoringApiError::TypeError(diagnostics) => (
                StatusCode::BAD_REQUEST,
                Json(WireError::invalid_input(diagnostics).with_error_type("TypeError")),
            )
                .into_response(),
            AuthoringApiError::Unavailable(wire) => {
                (StatusCode::SERVICE_UNAVAILABLE, Json(wire)).into_response()
            }
            AuthoringApiError::Wire(wire) => HttpWireError(wire).into_response(),
        }
    }
}

pub(crate) async fn compile_source(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<CompileSourceRequest>,
) -> Result<Json<CompileSourceResponse>, AuthoringHttpError> {
    authoring::compile_and_load(&state, &caller, TRANSPORT, request)
        .await
        .map(Json)
        .map_err(AuthoringHttpError)
}
