//! Native one-motion AWL compile, package, input validation, and result waiting.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use aion_awl_package::compile_and_assemble_awl;
use aion_client::WorkflowHandle;
use aion_core::{Event, Payload};
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::payload::{json_payload, payload_to_json};

/// A native AWL package ready for the unchanged operator deploy operation.
pub(crate) struct PreparedAwl {
    pub(crate) workflow_name: String,
    pub(crate) archive: Vec<u8>,
    pub(crate) input: Payload,
}

/// A stable client-side refusal emitted before any deploy connection is made.
#[derive(Debug)]
pub(crate) struct InputValidationError {
    expected_schema: String,
}

impl fmt::Display for InputValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "input validation failed: --input does not match the workflow input schema\nexpected schema: {}",
            self.expected_schema
        )
    }
}

impl std::error::Error for InputValidationError {}

/// Returns whether `path` names the case-sensitive `.awl` direct-compile path.
pub(crate) fn is_awl(path: &Path) -> bool {
    path.as_os_str().to_string_lossy().ends_with(".awl")
}

/// Reads, compiles, and assembles an AWL document without contacting a server.
pub(crate) fn package_file(path: &Path) -> Result<(String, Value, Vec<u8>)> {
    if !is_awl(path) {
        anyhow::bail!("run requires a .awl file: `{}`", path.display());
    }
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read AWL file `{}`", path.display()))?;
    package_source(&source, document_root(path))
}

/// Prepares one-motion run input. Validation completes before the caller can
/// hand `archive` to the deploy operation.
pub(crate) fn prepare_run(path: &Path, input: &str) -> Result<PreparedAwl> {
    let (workflow_name, input_schema, archive) = package_file(path)?;
    let input_value: Value = serde_json::from_str(input).context("invalid --input JSON")?;
    validate_input(&input_schema, &input_value)?;
    let input = json_payload(input).context("invalid --input JSON")?;
    Ok(PreparedAwl {
        workflow_name,
        archive,
        input,
    })
}

/// Compiles and assembles source into complete format-v1 package bytes.
fn package_source(source: &str, schema_root: &Path) -> Result<(String, Value, Vec<u8>)> {
    let prepared = compile_and_assemble_awl(source, schema_root)?;
    Ok((
        prepared.compiled.workflow_name,
        prepared.compiled.input_schema,
        prepared.archive,
    ))
}

/// Mirrors `aion awl check`: schema imports resolve against the document's
/// containing directory, with `.` for a bare relative filename.
fn document_root(file: &Path) -> &Path {
    match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn validate_input(schema: &Value, input: &Value) -> Result<()> {
    let validator = jsonschema::validator_for(schema)
        .context("compiled workflow produced an invalid input schema")?;
    if validator.is_valid(input) {
        return Ok(());
    }
    let expected_schema =
        serde_json::to_string(schema).context("failed to encode expected workflow input schema")?;
    Err(InputValidationError { expected_schema }.into())
}

/// How often the one-motion run re-reads the run's durable history while
/// waiting for a terminal event.
const RESULT_POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Waits for the concrete run's terminal event and emits the existing JSON
/// identifier fields plus its decoded result. The wait polls `describe` over
/// the one configured gRPC endpoint — event subscriptions need a separate
/// stream listener address the CLI does not require operators to provide.
/// AWL currently cannot continue as new, so that terminal is reported as a
/// refusal rather than silently detached.
pub(crate) async fn await_result(handle: WorkflowHandle, workflow_type: &str) -> Result<Value> {
    let workflow_id = handle.workflow_id().to_string();
    let run_id = handle.run_id().to_string();
    loop {
        let description = handle
            .describe()
            .await
            .context("failed while awaiting workflow result")?;
        for event in &description.history {
            match event {
                Event::WorkflowCompleted { result, .. } => {
                    return Ok(json!({
                        "workflow_type": workflow_type,
                        "workflow_id": workflow_id,
                        "run_id": run_id,
                        "result": payload_to_json(result)?,
                    }));
                }
                Event::WorkflowFailed { error, .. } => {
                    anyhow::bail!("workflow run failed: {error}");
                }
                Event::WorkflowCancelled { reason, .. } => {
                    anyhow::bail!("workflow run cancelled: {reason}");
                }
                Event::WorkflowTimedOut { timeout, .. } => {
                    anyhow::bail!("workflow run timed out: {timeout}");
                }
                Event::WorkflowContinuedAsNew { .. } => {
                    anyhow::bail!("workflow run continued as new before producing a result");
                }
                _ => {}
            }
        }
        tokio::time::sleep(RESULT_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use aion_package::{ExtractionLimits, Package};
    use serde_json::json;

    use super::{InputValidationError, package_file, package_source, prepare_run};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aion-awl/tests/fixtures/rev2")
            .join(relative)
    }

    #[test]
    fn known_good_corpus_fixture_compiles_and_assembles_without_a_server() -> TestResult {
        let (workflow_name, input_schema, bytes) =
            package_file(&fixture("flagship/valid/awl_hello.awl"))?;
        let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

        assert_eq!(workflow_name, "awl_hello");
        assert_eq!(package.manifest().entry_module, "awl_hello");
        assert_eq!(package.manifest().entry_function, "run");
        assert_eq!(package.manifest().input_schema, input_schema);
        assert!(package.beams().get("awl_hello").is_some());
        Ok(())
    }

    #[test]
    fn document_timeout_reaches_the_one_motion_archive_manifest() -> TestResult {
        let path = fixture("dag-fork/valid/after_single.awl");
        let source = std::fs::read_to_string(&path)?.replacen(
            "workflow after_single\n",
            "workflow after_single\n  timeout 6h\n",
            1,
        );
        let root = path.parent().ok_or("fixture has no parent directory")?;
        let (_, _, bytes) = package_source(&source, root)?;
        let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

        assert_eq!(
            package.manifest().timeout,
            std::time::Duration::from_secs(21_600)
        );
        Ok(())
    }

    #[test]
    fn mismatching_input_is_refused_before_an_archive_can_be_deployed() -> TestResult {
        let Err(error) = prepare_run(
            &fixture("flagship/valid/awl_hello.awl"),
            &json!({ "name": 42 }).to_string(),
        ) else {
            return Err("mismatching input unexpectedly passed validation".into());
        };
        let refusal = error
            .downcast_ref::<InputValidationError>()
            .ok_or("input mismatch did not preserve its typed refusal")?;
        let rendered = refusal.to_string();
        assert!(rendered.starts_with("input validation failed:"));
        assert!(rendered.contains("expected schema: {"));
        Ok(())
    }
}
