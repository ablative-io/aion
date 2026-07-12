use aion_proto::WireError;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use super::auth::HttpCaller;
use super::authoring::AuthoringHttpError;
use crate::ServerState;
use crate::awl::{
    self, CheckRequest, CheckResponse, CreateDocumentRequest, CreateDocumentResponse, Diagnostic,
    DocumentEntry, DocumentResponse, EditRequest, EditResponse, FormatRequest, FormatResponse,
    PutDocumentRequest,
};

pub(crate) async fn check(Json(request): Json<CheckRequest>) -> Json<CheckResponse> {
    Json(awl::check_source(&request))
}

pub(crate) async fn edit(Json(request): Json<EditRequest>) -> Json<EditResponse> {
    Json(awl::edit_source(&request))
}

pub(crate) async fn scaffold(
    Json(request): Json<awl::scaffold::ScaffoldRequest>,
) -> Json<awl::scaffold::ScaffoldResponse> {
    Json(awl::scaffold::scaffold(&request))
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

pub(crate) async fn create_document(
    State(state): State<ServerState>,
    Json(request): Json<CreateDocumentRequest>,
) -> Result<(StatusCode, Json<CreateDocumentResponse>), DocumentHttpError> {
    let root = workspace(&state)?;
    awl::documents::create(&root, request)
        .await
        .map(|response| (StatusCode::CREATED, Json(response)))
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

pub(crate) async fn get_layout(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(path): Path<String>,
) -> Result<Json<awl::layout::LayoutRecord>, LayoutHttpError> {
    let root = workspace(&state).map_err(|error| LayoutHttpError::not_configured(&error.0))?;
    awl::layout::read(&root, &path, caller.subject())
        .await
        .map(Json)
        .map_err(LayoutHttpError)
}

pub(crate) async fn put_layout(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(path): Path<String>,
    Json(request): Json<awl::layout::LayoutRecord>,
) -> Result<Json<awl::layout::LayoutRecord>, LayoutHttpError> {
    let root = workspace(&state).map_err(|error| LayoutHttpError::not_configured(&error.0))?;
    awl::layout::write(&root, &path, caller.subject(), request)
        .await
        .map(Json)
        .map_err(LayoutHttpError)
}

pub(crate) async fn emit(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<awl::run_loop::EmitRequest>,
) -> Result<Json<awl::run_loop::EmitResponse>, RunLoopHttpError> {
    awl::run_loop::emit(&state, &caller, &request)
        .map(Json)
        .map_err(RunLoopHttpError::RunLoop)
}

pub(crate) async fn deploy_authoring(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<awl::run_loop::DeployAuthoringRequest>,
) -> Result<Json<awl::run_loop::DeployAuthoringResponse>, RunLoopHttpError> {
    let root = workspace(&state).map_err(|error| RunLoopHttpError::Document(error.0))?;
    awl::run_loop::deploy(&state, &caller, &root, request)
        .await
        .map(Json)
        .map_err(RunLoopHttpError::RunLoop)
}

pub(crate) async fn get_revision(
    State(state): State<ServerState>,
    Path(hash): Path<String>,
) -> Result<Json<awl::revisions::Revision>, RunLoopHttpError> {
    let root = workspace(&state).map_err(|error| RunLoopHttpError::Document(error.0))?;
    awl::revisions::fetch(&root, &hash)
        .await
        .map(Json)
        .map_err(|error| RunLoopHttpError::RunLoop(error.into()))
}

pub(crate) async fn get_run_status(
    State(state): State<ServerState>,
    Path(deployment_id): Path<String>,
) -> Result<Json<awl::run_loop::RunStatusResponse>, RunLoopHttpError> {
    let root = workspace(&state).map_err(|error| RunLoopHttpError::Document(error.0))?;
    awl::run_loop::status(&root, &deployment_id)
        .await
        .map(Json)
        .map_err(RunLoopHttpError::RunLoop)
}

pub(crate) async fn bind_run(
    State(state): State<ServerState>,
    Path(deployment_id): Path<String>,
    Json(request): Json<awl::run_loop::BindRunRequest>,
) -> Result<Json<awl::revisions::DeploymentRecord>, RunLoopHttpError> {
    let root = workspace(&state).map_err(|error| RunLoopHttpError::Document(error.0))?;
    awl::revisions::bind_run(&root, &deployment_id, request.workflow_id, request.run_id)
        .await
        .map(Json)
        .map_err(|error| RunLoopHttpError::RunLoop(error.into()))
}

pub(crate) async fn worker_availability(
    State(state): State<ServerState>,
    Json(request): Json<awl::run_loop::WorkerAvailabilityRequest>,
) -> Result<Json<awl::run_loop::WorkerAvailabilityResponse>, RunLoopHttpError> {
    awl::run_loop::worker_availability(&state, request)
        .map(Json)
        .map_err(RunLoopHttpError::RunLoop)
}

pub(crate) enum RunLoopHttpError {
    Document(awl::documents::DocumentError),
    RunLoop(awl::run_loop::RunLoopError),
}

impl IntoResponse for RunLoopHttpError {
    fn into_response(self) -> Response {
        match self {
            Self::Document(error) => DocumentHttpError(error).into_response(),
            Self::RunLoop(awl::run_loop::RunLoopError::Authoring(error)) => {
                AuthoringHttpError(error).into_response()
            }
            Self::RunLoop(error) => {
                let status = match error {
                    awl::run_loop::RunLoopError::WorkerRegistry(_) => {
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                    _ => StatusCode::UNPROCESSABLE_ENTITY,
                };
                (status, Json(awl::run_loop::wire_error(&error))).into_response()
            }
        }
    }
}

fn workspace(state: &ServerState) -> Result<std::path::PathBuf, DocumentHttpError> {
    state
        .runtime_config()
        .authoring
        .workspace_dir
        .clone()
        .ok_or(DocumentHttpError(
            awl::documents::DocumentError::WorkspaceUnconfigured,
        ))
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

pub(crate) struct LayoutHttpError(pub(crate) awl::layout::LayoutError);

impl LayoutHttpError {
    fn not_configured(error: &awl::documents::DocumentError) -> Self {
        Self(awl::layout::LayoutError::DocumentNotFound(
            error.to_string(),
        ))
    }
}

impl IntoResponse for LayoutHttpError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match self.0 {
            awl::layout::LayoutError::InvalidPath(message) => {
                (StatusCode::BAD_REQUEST, "InvalidLayoutPath", message)
            }
            awl::layout::LayoutError::DocumentNotFound(message) => {
                (StatusCode::NOT_FOUND, "DocumentNotFound", message)
            }
            awl::layout::LayoutError::Io(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "LayoutIoError",
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

pub(crate) struct DocumentHttpError(pub(crate) awl::documents::DocumentError);

impl IntoResponse for DocumentHttpError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match self.0 {
            awl::documents::DocumentError::InvalidPath(message) => {
                (StatusCode::BAD_REQUEST, "InvalidDocumentPath", message)
            }
            awl::documents::DocumentError::InvalidName(message) => {
                (StatusCode::BAD_REQUEST, "InvalidDocumentName", message)
            }
            awl::documents::DocumentError::NotFound(message) => {
                (StatusCode::NOT_FOUND, "DocumentNotFound", message)
            }
            awl::documents::DocumentError::Exists(message) => {
                (StatusCode::CONFLICT, "DocumentExists", message)
            }
            awl::documents::DocumentError::WorkspaceUnconfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "AuthoringWorkspaceUnconfigured",
                "AWL workspace is not configured".to_owned(),
            ),
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
