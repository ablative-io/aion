use std::path::Path;

use aion_awl_package::AwlAssembleOptions;
use aion_proto::WireError;
use serde::{Deserialize, Serialize};

use super::handlers::{CheckRequest, check_source};
use super::revisions::{self, DeploymentRecord, RevisionError};
use crate::authoring::{AuthoringApiError, CompileSourceRequest, compile_and_load_document};
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

struct PreparedEmission {
    source: String,
    timeout: Option<std::time::Duration>,
}

fn prepare_emission(
    document: &aion_awl::Document,
    root: &Path,
) -> Result<PreparedEmission, RunLoopError> {
    let timeout = document
        .timeout
        .as_ref()
        .map(|timeout| {
            timeout.duration.checked_duration().ok_or_else(|| {
                RunLoopError::CheckRefused("workflow `timeout` is too large".to_owned())
            })
        })
        .transpose()?;
    let source = aion_awl::emit_in(document, root)
        .map_err(|error| RunLoopError::EmitRefused(error.message))?;
    Ok(PreparedEmission { source, timeout })
}

/// Selects the queue recorded for ship-and-run from the document itself.
///
/// AWL has no separate workflow-level task-queue header: worker declaration
/// names are task queues, so the first declaration is the document's launch
/// queue. Workerless workflows use the server's canonical default queue. The
/// authoring template's frozen manifest is intentionally not consulted because
/// it describes build policy, not the deployed document's workers.
fn document_task_queue(document: &aion_awl::Document) -> String {
    document.workers.first().map_or_else(
        || DEFAULT_TASK_QUEUE.to_owned(),
        |worker| worker.name.clone(),
    )
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
    let workflow_type = document.name.clone();
    let task_queue = document_task_queue(&document);
    let prepared = prepare_emission(&document, root)?;
    let loaded = compile_and_load_document(
        state,
        caller,
        "http",
        CompileSourceRequest {
            source: prepared.source.clone(),
        },
        workflow_type.clone(),
        AwlAssembleOptions {
            timeout: prepared.timeout,
        },
    )
    .await?;
    let deployment = DeploymentRecord {
        deployment_id: uuid::Uuid::new_v4().to_string(),
        document_path: request.path,
        content_hash: revision.content_hash,
        package_id: loaded.content_hash.clone(),
        workflow_type,
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
                step: "emit",
                detail: format!("{} bytes of Gleam emitted", prepared.source.len()),
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
        RunLoopError::Revision(_) => "RevisionStore",
        RunLoopError::Authoring(_) => "AuthoringDeploy",
        RunLoopError::WorkerRegistry(_) => "WorkerRegistry",
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use aion_awl_package::AwlAssembleOptions;
    use aion_package::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ExtractionLimits, Manifest, ManifestVersion,
        Package, PackageBuilder,
    };
    use serde_json::json;

    use super::prepare_emission;
    use crate::authoring::handlers::package_with_options;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aion-awl/tests/fixtures/rev2")
            .join(relative)
    }

    #[test]
    fn emitter_deploy_preparation_accepts_native_refused_fixtures() -> TestResult {
        for relative in [
            "flagship/valid/dev_brief.awl",
            "loop-outcomes/valid/guard_optional_wait.awl",
            "loop-outcomes/valid/ship_release_combined.awl",
        ] {
            let path = fixture(relative);
            let source = std::fs::read_to_string(&path)?;
            let root = path.parent().ok_or("fixture has no parent directory")?;
            assert!(
                aion_awl::compile(&source, root).is_err(),
                "fixture is no longer a native refusal: {relative}"
            );
            let document = aion_awl::parse(&source)?;
            let prepared = prepare_emission(&document, root)?;
            assert!(
                !prepared.source.is_empty(),
                "emitter produced no source for {relative}"
            );
        }
        Ok(())
    }

    #[test]
    fn emitter_deploy_timeout_reaches_the_manifest() -> TestResult {
        let path = fixture("header-types/valid/workflow_timeout.awl");
        let source = std::fs::read_to_string(&path)?;
        let root = path.parent().ok_or("fixture has no parent directory")?;
        let document = aion_awl::parse(&source)?;
        let prepared = prepare_emission(&document, root)?;
        let package = basic_package()?;
        let adjusted = package_with_options(
            package,
            AwlAssembleOptions {
                timeout: prepared.timeout,
            },
        )
        .map_err(|error| format!("package timeout override failed: {error:?}"))?;
        assert_eq!(
            adjusted.manifest().timeout,
            std::time::Duration::from_secs(21_600)
        );
        Ok(())
    }

    fn basic_package() -> TestResultPackage {
        let beams = BeamSet::new(vec![BeamModule::new("server_timeout", b"opaque".to_vec())])?;
        let manifest = Manifest {
            entry_module: "server_timeout".to_owned(),
            entry_function: "run".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            timeout: std::time::Duration::from_secs(30),
            activities: Vec::new(),
            version: ManifestVersion::new("unstamped"),
            format_version: CURRENT_FORMAT_VERSION,
        };
        let bytes = PackageBuilder::new(manifest, beams).write_to_bytes()?;
        Ok(Package::load_from_bytes(
            bytes,
            ExtractionLimits::unbounded(),
        )?)
    }

    type TestResultPackage = Result<Package, Box<dyn std::error::Error>>;
}
