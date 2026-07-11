use aion_proto::WireError;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::ServerState;
use crate::awl::{
    self, CheckRequest, CheckResponse, Diagnostic, DocumentEntry, DocumentResponse, FormatRequest,
    FormatResponse, PutDocumentRequest,
};

pub(crate) async fn check(Json(request): Json<CheckRequest>) -> Json<CheckResponse> {
    Json(awl::check_source(&request))
}

pub(crate) async fn format(
    Json(request): Json<FormatRequest>,
) -> Result<Json<FormatResponse>, FormatHttpError> {
    awl::format_source(&request)
        .map(Json)
        .map_err(|diagnostic| FormatHttpError { diagnostic })
}

pub(crate) async fn list_documents(
    State(state): State<ServerState>,
) -> Result<Json<Vec<DocumentEntry>>, DocumentHttpError> {
    let root = workspace(&state)?;
    awl::documents::list(&root)
        .await
        .map(Json)
        .map_err(DocumentHttpError)
}

pub(crate) async fn get_document(
    State(state): State<ServerState>,
    Path(path): Path<String>,
) -> Result<Json<DocumentResponse>, DocumentHttpError> {
    let root = workspace(&state)?;
    awl::documents::read(&root, &path)
        .await
        .map(Json)
        .map_err(DocumentHttpError)
}

pub(crate) async fn put_document(
    State(state): State<ServerState>,
    Path(path): Path<String>,
    Json(request): Json<PutDocumentRequest>,
) -> Result<Json<DocumentResponse>, DocumentHttpError> {
    let root = workspace(&state)?;
    awl::documents::write(&root, &path, request)
        .await
        .map(Json)
        .map_err(DocumentHttpError)
}

fn workspace(state: &ServerState) -> Result<std::path::PathBuf, DocumentHttpError> {
    state
        .runtime_config()
        .authoring
        .workspace_dir
        .clone()
        .ok_or_else(|| {
            DocumentHttpError(awl::documents::DocumentError::NotFound(
                "AWL workspace is not configured".to_owned(),
            ))
        })
}

pub(crate) struct FormatHttpError {
    diagnostic: Diagnostic,
}

#[derive(Serialize)]
struct DiagnosticsBody {
    diagnostics: Vec<Diagnostic>,
}

impl IntoResponse for FormatHttpError {
    fn into_response(self) -> Response {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(DiagnosticsBody {
                diagnostics: vec![self.diagnostic],
            }),
        )
            .into_response()
    }
}

pub(crate) struct DocumentHttpError(pub(crate) awl::documents::DocumentError);

impl IntoResponse for DocumentHttpError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match self.0 {
            awl::documents::DocumentError::InvalidPath(message) => {
                (StatusCode::BAD_REQUEST, "InvalidDocumentPath", message)
            }
            awl::documents::DocumentError::NotFound(message) => {
                (StatusCode::NOT_FOUND, "DocumentNotFound", message)
            }
            awl::documents::DocumentError::Io(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DocumentIoError",
                error.to_string(),
            ),
        };
        (
            status,
            Json(WireError::invalid_input(message).with_error_type(error_type)),
        )
            .into_response()
    }
}
