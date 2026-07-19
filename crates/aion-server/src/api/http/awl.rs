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
use super::error::HttpWireError;
use crate::ServerState;
use crate::awl::{
    self, CheckRequest, CheckResponse, CreateDocumentRequest, CreateDocumentResponse, Diagnostic,
    DocumentEntry, DocumentResponse, EditRequest, EditResponse, FormatRequest, FormatResponse,
    PutDocumentRequest,
};

pub(crate) async fn check(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<CheckRequest>,
) -> Result<Json<CheckResponse>, Response> {
    require_authenticated(&caller)?;
    let root = workspace(&state).map_err(IntoResponse::into_response)?;
    awl::check_source_in_workspace(&root, &request)
        .await
        .map(Json)
        .map_err(|error| DocumentHttpError(error).into_response())
}

pub(crate) async fn edit(
    HttpCaller(caller): HttpCaller,
    Json(request): Json<EditRequest>,
) -> Result<Json<EditResponse>, Response> {
    require_authenticated(&caller)?;
    Ok(Json(awl::edit_source(&request)))
}

pub(crate) async fn scaffold(
    HttpCaller(caller): HttpCaller,
    Json(request): Json<awl::scaffold::ScaffoldRequest>,
) -> Result<Json<awl::scaffold::ScaffoldResponse>, Response> {
    require_authenticated(&caller)?;
    Ok(Json(awl::scaffold::scaffold(&request)))
}

pub(crate) async fn format(
    HttpCaller(caller): HttpCaller,
    Json(request): Json<FormatRequest>,
) -> Result<Json<FormatResponse>, Response> {
    require_authenticated(&caller)?;
    awl::format_source(&request)
        .map(Json)
        .map_err(|diagnostic| FormatHttpError { diagnostic }.into_response())
}

pub(crate) async fn list_documents(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
) -> Result<Json<Vec<DocumentEntry>>, Response> {
    require_authenticated(&caller)?;
    let root = workspace(&state).map_err(IntoResponse::into_response)?;
    awl::documents::list(&root)
        .await
        .map(Json)
        .map_err(|error| DocumentHttpError(error).into_response())
}

pub(crate) async fn create_document(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<CreateDocumentRequest>,
) -> Result<(StatusCode, Json<CreateDocumentResponse>), Response> {
    require_mutation(&state, &caller)?;
    let root = workspace(&state).map_err(IntoResponse::into_response)?;
    awl::documents::create(&root, request)
        .await
        .map(|response| (StatusCode::CREATED, Json(response)))
        .map_err(|error| DocumentHttpError(error).into_response())
}

pub(crate) async fn get_document(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(path): Path<String>,
) -> Result<Json<DocumentResponse>, Response> {
    require_authenticated(&caller)?;
    let root = workspace(&state).map_err(IntoResponse::into_response)?;
    awl::documents::read(&root, &path)
        .await
        .map(Json)
        .map_err(|error| DocumentHttpError(error).into_response())
}

pub(crate) async fn put_document(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(path): Path<String>,
    Json(request): Json<PutDocumentRequest>,
) -> Result<Json<DocumentResponse>, Response> {
    require_mutation(&state, &caller)?;
    let root = workspace(&state).map_err(IntoResponse::into_response)?;
    awl::documents::write(&root, &path, request)
        .await
        .map(Json)
        .map_err(|error| DocumentHttpError(error).into_response())
}

pub(crate) async fn get_layout(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(path): Path<String>,
) -> Result<Json<awl::layout::LayoutRecord>, Response> {
    require_authenticated(&caller)?;
    let root = workspace(&state)
        .map_err(|error| LayoutHttpError::not_configured(&error.0).into_response())?;
    awl::layout::read(&root, &path, caller.subject())
        .await
        .map(Json)
        .map_err(|error| LayoutHttpError(error).into_response())
}

pub(crate) async fn put_layout(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(path): Path<String>,
    Json(request): Json<awl::layout::LayoutRecord>,
) -> Result<Json<awl::layout::LayoutRecord>, Response> {
    require_mutation(&state, &caller)?;
    let root = workspace(&state)
        .map_err(|error| LayoutHttpError::not_configured(&error.0).into_response())?;
    awl::layout::write(&root, &path, caller.subject(), request)
        .await
        .map(Json)
        .map_err(|error| LayoutHttpError(error).into_response())
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
    HttpCaller(caller): HttpCaller,
    Path(hash): Path<String>,
) -> Result<Json<awl::revisions::Revision>, Response> {
    require_authenticated(&caller)?;
    let root =
        workspace(&state).map_err(|error| RunLoopHttpError::Document(error.0).into_response())?;
    awl::revisions::fetch(&root, &hash)
        .await
        .map(Json)
        .map_err(|error| RunLoopHttpError::RunLoop(error.into()).into_response())
}

pub(crate) async fn get_run_status(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(deployment_id): Path<String>,
) -> Result<Json<awl::run_loop::RunStatusResponse>, Response> {
    require_authenticated(&caller)?;
    let root =
        workspace(&state).map_err(|error| RunLoopHttpError::Document(error.0).into_response())?;
    awl::run_loop::status(&root, &deployment_id)
        .await
        .map(Json)
        .map_err(|error| RunLoopHttpError::RunLoop(error).into_response())
}

pub(crate) async fn bind_run(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(deployment_id): Path<String>,
    Json(request): Json<awl::run_loop::BindRunRequest>,
) -> Result<Json<awl::revisions::DeploymentRecord>, Response> {
    require_mutation(&state, &caller)?;
    let root =
        workspace(&state).map_err(|error| RunLoopHttpError::Document(error.0).into_response())?;
    awl::revisions::bind_run(&root, &deployment_id, request.workflow_id, request.run_id)
        .await
        .map(Json)
        .map_err(|error| RunLoopHttpError::RunLoop(error.into()).into_response())
}

pub(crate) async fn worker_availability(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<awl::run_loop::WorkerAvailabilityRequest>,
) -> Result<Json<awl::run_loop::WorkerAvailabilityResponse>, Response> {
    require_authenticated(&caller)?;
    awl::run_loop::worker_availability(&state, request)
        .map(Json)
        .map_err(|error| RunLoopHttpError::RunLoop(error).into_response())
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

struct AwlAuthorizationError(WireError);

impl From<AwlAuthorizationError> for Response {
    fn from(error: AwlAuthorizationError) -> Self {
        HttpWireError(error.0).into_response()
    }
}

fn require_authenticated(caller: &crate::CallerIdentity) -> Result<(), AwlAuthorizationError> {
    if let Some(reason) = caller.denial_reason() {
        return Err(AwlAuthorizationError(WireError::namespace_denied(format!(
            "AWL studio requires an authenticated caller: {reason}"
        ))));
    }
    Ok(())
}

fn require_mutation(
    state: &ServerState,
    caller: &crate::CallerIdentity,
) -> Result<(), AwlAuthorizationError> {
    state
        .deploy_guard()
        .authorize(caller)
        .map_err(|error| AwlAuthorizationError(error.to_wire_error()))
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
