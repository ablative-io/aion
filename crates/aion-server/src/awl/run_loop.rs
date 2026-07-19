use std::path::Path;

use aion_awl_package::{PrepareAwlError, compile_and_assemble_awl};
use aion_core::DEFAULT_TASK_QUEUE;
use aion_package::{ExtractionLimits, Package, PackageError};
use aion_proto::WireError;
use serde::{Deserialize, Serialize};

use super::handlers::{CheckRequest, check_source};
use super::revisions::{self, DeploymentRecord, RevisionError};
use crate::authoring::AuthoringApiError;
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
    pub synthesized_workflows: Vec<EmittedWorkflowEntry>,
}

#[derive(Debug, Serialize)]
pub struct EmittedWorkflowEntry {
    pub workflow_type: String,
    pub entry_module: String,
    pub entry_function: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub timeout_seconds: u64,
    pub internal: bool,
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
    #[error(transparent)]
    Direct(#[from] PrepareAwlError),
    #[error("direct AWL package could not be loaded: {0}")]
    Package(#[from] PackageError),
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
    // `/awl/emit` accepts unsaved source, so request.path is a display label,
    // never ambient filesystem authority. Imported schemas intentionally refuse
    // here; saved-document deploy stages them through the workspace capability.
    let artifact = aion_awl::emit_artifact(&document)
        .map_err(|error| RunLoopError::EmitRefused(error.message))?;
    let synthesized_workflows = artifact
        .synthesized_workflows
        .into_iter()
        .map(|entry| EmittedWorkflowEntry {
            workflow_type: entry.workflow_type,
            entry_module: entry.entry_module,
            entry_function: entry.entry_function,
            input_schema: entry.input_schema,
            output_schema: entry.output_schema,
            timeout_seconds: entry.timeout_seconds,
            internal: entry.internal,
        })
        .collect();
    Ok(EmitResponse {
        bytes: artifact.source.len(),
        emitted: artifact.source,
        synthesized_workflows,
    })
}

pub async fn deploy(
    state: &ServerState,
    caller: &CallerIdentity,
    root: &Path,
    request: DeployAuthoringRequest,
) -> Result<DeployAuthoringResponse, RunLoopError> {
    crate::authoring::handlers::admit_mutation(state, caller, "http", "awl.deploy")?;
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
    let workspace_root = root.to_owned();
    let document_path = request.path.clone();
    let revision_source = revision.source.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let (_staging, schema_root) = super::handlers::stage_schema_imports(
            &workspace_root,
            &document_path,
            &revision_source,
        )
        .map_err(|error| RevisionError::InvalidRecord(error.to_string()))?;
        compile_and_assemble_awl(&revision_source, &schema_root).map_err(RunLoopError::from)
    })
    .await
    .map_err(|error| {
        RunLoopError::Revision(RevisionError::InvalidRecord(format!(
            "AWL compile task failed: {error}"
        )))
    })??;
    let task_queue = match &prepared.compiled.first_worker {
        Some(worker) => worker.clone(),
        None => DEFAULT_TASK_QUEUE.to_owned(),
    };
    let workflow_name = prepared.compiled.workflow_name.clone();
    let beam_bytes = prepared.compiled.beam_bytes.len();
    let package = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;
    crate::authoring::handlers::validate_document_identity(&package, &workflow_name)?;
    let loaded = crate::authoring::handlers::load_admitted_package(
        state,
        caller,
        "http",
        "awl.deploy",
        package,
    )
    .await?;
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
                detail: format!("direct compiler accepted workflow {workflow_name}"),
            },
            GuidedStepResult {
                step: "compile",
                detail: format!("{beam_bytes} bytes of direct BEAM compiled"),
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
        | RunLoopError::Direct(_)
        | RunLoopError::Package(_)
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
        RunLoopError::Direct(_) => "DirectCompile",
        RunLoopError::Package(_) => "Package",
        RunLoopError::Revision(_) => "RevisionStore",
        RunLoopError::Authoring(_) => "AuthoringDeploy",
        RunLoopError::WorkerRegistry(_) => "WorkerRegistry",
    }
}
