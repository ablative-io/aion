use std::path::Path;

use aion_awl_package::{AwlAssembleOptions, assemble_awl};
use aion_proto::WireError;
use serde::{Deserialize, Serialize};

use super::handlers::{CheckRequest, check_source};
use super::revisions::{self, DeploymentRecord, RevisionError};
use crate::authoring::AuthoringApiError;
use crate::worker::registry::DEFAULT_TASK_QUEUE;
use crate::{CallerIdentity, ServerState};

#[derive(Debug, Deserialize)]
pub struct EmitRequest {
    pub source: String,
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EmitResponse {
    pub emitted: String,
    pub bytes: usize,
}

#[derive(Debug, Deserialize)]
pub struct DeployAuthoringRequest {
    pub path: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct GuidedStepResult {
    pub step: &'static str,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeployAuthoringResponse {
    pub deployment: DeploymentRecord,
    pub steps: Vec<GuidedStepResult>,
}

#[derive(Debug, Deserialize)]
pub struct BindRunRequest {
    pub workflow_id: String,
    pub run_id: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkerAvailabilityRequest {
    pub namespace: String,
    pub task_queue: String,
}

#[derive(Debug, Serialize)]
pub struct WorkerAvailabilityResponse {
    pub available: bool,
    pub task_queue: String,
    pub connected_workers: usize,
    pub scaffold_hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunStatusResponse {
    pub deployment: DeploymentRecord,
    pub deployed_source: String,
    pub drifted: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RunLoopError {
    #[error(
        "document revision does not match the saved document: requested {requested}, saved {saved}"
    )]
    RevisionMismatch { requested: String, saved: String },
    #[error("AWL check refused deployment: {0}")]
    CheckRefused(String),
    #[error("AWL emission refused deployment: {0}")]
    EmitRefused(String),
    #[error("AWL native compilation refused deployment: {0}")]
    CompileRefused(String),
    #[error(transparent)]
    Revision(#[from] RevisionError),
    #[error("authoring deploy was refused")]
    Authoring(AuthoringApiError),
    #[error("worker registry inspection failed: {0}")]
    WorkerRegistry(String),
}

pub fn emit(
    state: &ServerState,
    caller: &CallerIdentity,
    request: &EmitRequest,
) -> Result<EmitResponse, RunLoopError> {
    state
        .deploy_guard()
        .authorize(caller)
        .map_err(|error| RunLoopError::Authoring(AuthoringApiError::Wire(error.to_wire_error())))?;
    let checked = check_source(&CheckRequest {
        source: request.source.clone(),
        path: request.path.clone(),
    });
    if !checked.deploys_green {
        let reason = checked.diagnostics.first().map_or_else(
            || "document does not deploy green".to_owned(),
            |item| item.message.clone(),
        );
        return Err(RunLoopError::CheckRefused(reason));
    }
    let document = aion_awl::parse(&request.source)
        .map_err(|error| RunLoopError::CheckRefused(error.message))?;
    let root = request
        .path
        .as_deref()
        .and_then(|path| Path::new(path).parent());
    let emitted = root
        .map_or_else(
            || aion_awl::emit(&document),
            |root| aion_awl::emit_in(&document, root),
        )
        .map_err(|error| RunLoopError::EmitRefused(error.message))?;
    Ok(EmitResponse {
        bytes: emitted.len(),
        emitted,
    })
}

pub async fn deploy(
    state: &ServerState,
    caller: &CallerIdentity,
    root: &Path,
    request: DeployAuthoringRequest,
) -> Result<DeployAuthoringResponse, RunLoopError> {
    let saved = super::documents::read(root, &request.path)
        .await
        .map_err(|error| RevisionError::InvalidRecord(error.to_string()))?;
    if saved.content_hash != request.content_hash {
        return Err(RunLoopError::RevisionMismatch {
            requested: request.content_hash,
            saved: saved.content_hash,
        });
    }
    let revision = revisions::store(root, &saved.source).await?;
    let checked = check_source(&CheckRequest {
        source: revision.source.clone(),
        path: Some(root.join(&request.path).to_string_lossy().into_owned()),
    });
    if !checked.deploys_green {
        let reason = checked.diagnostics.first().map_or_else(
            || "document does not deploy green".to_owned(),
            |item| item.message.clone(),
        );
        return Err(RunLoopError::CheckRefused(reason));
    }
    let document = aion_awl::parse(&revision.source)
        .map_err(|error| RunLoopError::CheckRefused(error.message))?;
    let task_queue = document.workers.first().map_or_else(
        || DEFAULT_TASK_QUEUE.to_owned(),
        |worker| worker.name.clone(),
    );
    let document_path = root.join(&request.path);
    let schema_root = document_path.parent().map_or(root, |parent| parent);
    let compiled = aion_awl::compile(&revision.source, schema_root)
        .map_err(|error| RunLoopError::CompileRefused(error.to_string()))?;
    let archive = assemble_awl(
        &compiled,
        AwlAssembleOptions {
            timeout: compiled.timeout,
        },
    )
    .map_err(|error| RunLoopError::CompileRefused(error.to_string()))?;
    let loaded = crate::api::handlers::deploy::load_package(state, caller, "http", archive)
        .await
        .map_err(|error| RunLoopError::Authoring(AuthoringApiError::Wire(error.wire().clone())))?;
    let deployment = DeploymentRecord {
        deployment_id: uuid::Uuid::new_v4().to_string(),
        document_path: request.path,
        content_hash: revision.content_hash,
        package_id: loaded.content_hash.clone(),
        workflow_type: loaded.workflow_type.clone(),
        task_queue,
        workflow_id: None,
        run_id: None,
    };
    revisions::record_deployment(root, &deployment).await?;
    Ok(DeployAuthoringResponse {
        steps: vec![
            GuidedStepResult {
                step: "check",
                detail: format!("{} steps deploy green", checked.steps.unwrap_or(0)),
            },
            GuidedStepResult {
                step: "compile",
                detail: format!(
                    "{} bytes of native AWL bytecode compiled",
                    compiled.beam_bytes.len()
                ),
            },
            GuidedStepResult {
                step: "package",
                detail: format!("package {} built", loaded.content_hash),
            },
            GuidedStepResult {
                step: "deploy",
                detail: format!("deployment {} loaded", deployment.deployment_id),
            },
        ],
        deployment,
    })
}

pub fn worker_availability(
    state: &ServerState,
    request: WorkerAvailabilityRequest,
) -> Result<WorkerAvailabilityResponse, RunLoopError> {
    let workers = state
        .worker_registry()
        .all_workers()
        .map_err(|error| RunLoopError::WorkerRegistry(error.to_string()))?;
    let task_queue = if request.task_queue.is_empty() {
        DEFAULT_TASK_QUEUE.to_owned()
    } else {
        request.task_queue
    };
    let connected_workers = workers
        .iter()
        .filter(|worker| {
            worker.task_queue() == task_queue && worker.namespaces().contains(&request.namespace)
        })
        .count();
    Ok(WorkerAvailabilityResponse {
        available: connected_workers > 0,
        task_queue: task_queue.clone(),
        connected_workers,
        scaffold_hint: (connected_workers == 0).then(|| {
            format!("No connected worker serves task queue `{task_queue}`. Scaffold and run this worker from Workers & Actions, then retry start.")
        }),
    })
}

pub async fn status(root: &Path, deployment_id: &str) -> Result<RunStatusResponse, RunLoopError> {
    let deployment = revisions::deployment(root, deployment_id).await?;
    let revision = revisions::fetch(root, &deployment.content_hash).await?;
    let drifted = revisions::current_drifted(root, &deployment).await?;
    Ok(RunStatusResponse {
        deployment,
        deployed_source: revision.source,
        drifted,
    })
}

pub fn wire_error(error: &RunLoopError) -> WireError {
    match error {
        RunLoopError::RevisionMismatch { .. }
        | RunLoopError::CheckRefused(_)
        | RunLoopError::EmitRefused(_)
        | RunLoopError::CompileRefused(_)
        | RunLoopError::Revision(_) => {
            WireError::invalid_input(error.to_string()).with_error_type(error_type(error))
        }
        RunLoopError::Authoring(_) | RunLoopError::WorkerRegistry(_) => {
            WireError::backend(error.to_string()).with_error_type(error_type(error))
        }
    }
}

impl From<AuthoringApiError> for RunLoopError {
    fn from(error: AuthoringApiError) -> Self {
        Self::Authoring(error)
    }
}

fn error_type(error: &RunLoopError) -> &'static str {
    match error {
        RunLoopError::RevisionMismatch { .. } => "RevisionMismatch",
        RunLoopError::CheckRefused(_) => "CheckRefused",
        RunLoopError::EmitRefused(_) => "EmitRefused",
        RunLoopError::CompileRefused(_) => "CompileRefused",
        RunLoopError::Revision(_) => "RevisionStore",
        RunLoopError::Authoring(_) => "AuthoringDeploy",
        RunLoopError::WorkerRegistry(_) => "WorkerRegistry",
    }
}
