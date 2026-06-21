//! Local `check --deterministic` subcommand: the static determinism gate.
//!
//! The determinism boundary (invariant 2) requires workflow code to be a pure
//! function of its recorded history — time from `workflow.now`, entropy from
//! `workflow.random` — so a replay re-runs the body and returns identical
//! commands. A direct wall-clock read or entropy draw escapes the recorder and
//! diverges on replay. `aion check --deterministic` is the CI gate that catches
//! that statically (P7, C28): it walks each declared workflow's entry-module
//! source from its entry function over the same call graph the structure
//! extractor uses, flags every wall-clock or entropy call reachable from
//! workflow code, and exits non-zero when any is found — so a tainted workflow
//! fails the build instead of desyncing in production.
//!
//! The analysis itself lives in `aion_package::structure` (it reuses the
//! tokeniser and call-graph walking the graph projection is built on); this
//! command is the thin local driver that reads the project's `workflow.toml`,
//! feeds each workflow's source in, and turns the findings into a report and an
//! exit code.

use std::fs;
use std::path::Path;

use aion_package::{Violation, ViolationKind, analyze_determinism};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use toml_edit::DocumentMut;

use crate::output::to_value;

/// One workflow declared in `workflow.toml`, with the source location the gate
/// reads.
struct WorkflowTarget {
    /// Logical entry-module name (`src/<entry_module>.gleam`).
    entry_module: String,
    /// The engine entry function the analysis walks from.
    entry_function: String,
}

/// A flagged call rendered for the JSON report.
#[derive(Serialize)]
struct ViolationReport {
    /// The workflow's entry module.
    workflow: String,
    /// The function the call was found in.
    function: String,
    /// The fully-qualified call (`erlang.system_time`).
    call: String,
    /// `wall_clock` or `entropy`.
    kind: &'static str,
    /// The deterministic SDK substitute to use instead.
    remedy: &'static str,
}

/// The `check --deterministic` JSON document printed on a clean pass.
#[derive(Serialize)]
struct CheckOutput {
    /// The workflows that were analysed, by entry module.
    workflows: Vec<String>,
    /// Always `true` here — a violation is an error, never a passing document.
    deterministic: bool,
}

/// Runs the `check --deterministic` gate over the workflow project at `path`.
///
/// Returns the clean-pass report on success, or a loud error enumerating every
/// flagged call (which the CLI renders to stderr and exits non-zero on, making
/// this usable as a CI gate, C28).
pub(crate) fn run(path: &Path, deterministic: bool) -> Result<Value> {
    if !deterministic {
        bail!(
            "`aion check` currently performs the determinism gate only; pass `--deterministic` \
             to run it"
        );
    }

    let targets = read_targets(path)?;
    let mut all_violations: Vec<(String, Violation)> = Vec::new();
    let mut analysed: Vec<String> = Vec::new();
    for target in &targets {
        let source = read_entry_source(path, &target.entry_module)?;
        let violations =
            analyze_determinism(&source, &target.entry_function).with_context(|| {
                format!(
                    "failed to analyse workflow `{}` for determinism",
                    target.entry_module
                )
            })?;
        analysed.push(target.entry_module.clone());
        for violation in violations {
            all_violations.push((target.entry_module.clone(), violation));
        }
    }

    if all_violations.is_empty() {
        return to_value(CheckOutput {
            workflows: analysed,
            deterministic: true,
        });
    }

    // A violation is a gate failure: render every flagged call and fail loudly so
    // the CLI exits non-zero. The JSON detail rides in the error for callers that
    // capture it.
    let reports: Vec<ViolationReport> = all_violations
        .iter()
        .map(|(workflow, violation)| ViolationReport {
            workflow: workflow.clone(),
            function: violation.function.clone(),
            call: violation.call.clone(),
            kind: kind_tag(violation.kind),
            remedy: violation.kind.remedy(),
        })
        .collect();
    let detail = serde_json::to_string_pretty(&reports)
        .unwrap_or_else(|_| format!("{} determinism violation(s)", reports.len()));
    bail!(
        "determinism check failed: {} non-deterministic call(s) reachable from workflow code. \
         Workflow code must read time via `workflow.now()` and entropy via `workflow.random()` \
         so replay is exact.\n{detail}",
        all_violations.len()
    );
}

/// The lowercase wire tag for a violation kind.
fn kind_tag(kind: ViolationKind) -> &'static str {
    match kind {
        ViolationKind::WallClock => "wall_clock",
        ViolationKind::Entropy => "entropy",
    }
}

/// Reads every `[[workflow]]` entry's module and entry function from
/// `workflow.toml`.
fn read_targets(root: &Path) -> Result<Vec<WorkflowTarget>> {
    let toml_path = root.join("workflow.toml");
    let contents = fs::read_to_string(&toml_path)
        .with_context(|| format!("failed to read {}", toml_path.display()))?;
    let document = contents
        .parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", toml_path.display()))?;
    let workflows = document
        .get("workflow")
        .and_then(|item| item.as_array_of_tables())
        .with_context(|| {
            format!(
                "{} declares no [[workflow]] entry to check",
                toml_path.display()
            )
        })?;
    let mut targets = Vec::with_capacity(workflows.len());
    for table in workflows {
        let entry_module = table
            .get("entry_module")
            .and_then(|item| item.as_str())
            .with_context(|| {
                format!(
                    "{} has a [[workflow]] entry with no `entry_module`",
                    toml_path.display()
                )
            })?;
        let entry_function = table
            .get("entry_function")
            .and_then(|item| item.as_str())
            .with_context(|| {
                format!(
                    "{} [[workflow]] `{entry_module}` has no `entry_function`",
                    toml_path.display()
                )
            })?;
        targets.push(WorkflowTarget {
            entry_module: entry_module.to_owned(),
            entry_function: entry_function.to_owned(),
        });
    }
    Ok(targets)
}

/// Reads the verbatim `src/<entry_module>.gleam` source of one workflow.
fn read_entry_source(root: &Path, entry_module: &str) -> Result<String> {
    let path = root.join("src").join(format!("{entry_module}.gleam"));
    fs::read_to_string(&path)
        .with_context(|| format!("failed to read workflow source {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const WORKFLOW_TOML: &str = "[[workflow]]\n\
         entry_module = \"demo\"\n\
         entry_function = \"run\"\n\
         input_schema = \"schemas/input.json\"\n\
         output_schema = \"schemas/output.json\"\n\
         activities = []\n";

    /// A clean workflow: time and entropy come only through the recorded
    /// `workflow.*` surface.
    const CLEAN_SOURCE: &str = "import aion/workflow\n\
         pub fn run(input) {\n  \
         let assert Ok(_) = workflow.now()\n  \
         let assert Ok(_) = workflow.random()\n  \
         workflow.run(wrappers.charge_activity(input))\n}\n";

    /// A tainted workflow: a direct wall-clock read escapes the recorder.
    const TAINTED_SOURCE: &str = "import aion/workflow\n\
         pub fn run(input) {\n  \
         let _ = erlang.system_time(1000)\n  \
         workflow.run(wrappers.charge_activity(input))\n}\n";

    fn project(
        label: &str,
        source: &str,
    ) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("aion-check-determinism-{label}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join("workflow.toml"), WORKFLOW_TOML)?;
        fs::write(root.join("src/demo.gleam"), source)?;
        Ok(root)
    }

    #[test]
    fn negative_fixture_clean_workflow_passes() -> TestResult {
        let root = project("clean", CLEAN_SOURCE)?;
        let value = run(&root, true)?;
        assert_eq!(value["deterministic"], serde_json::json!(true));
        assert_eq!(value["workflows"], serde_json::json!(["demo"]));
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn positive_fixture_tainted_workflow_fails_non_zero() -> TestResult {
        let root = project("tainted", TAINTED_SOURCE)?;
        let result = run(&root, true);
        let Err(error) = result else {
            fs::remove_dir_all(&root)?;
            return Err("a tainted workflow must fail the determinism gate".into());
        };
        let message = format!("{error}");
        assert!(
            message.contains("determinism check failed"),
            "error must name the gate failure: {message}"
        );
        assert!(
            message.contains("erlang.system_time"),
            "error must name the offending call: {message}"
        );
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn without_the_flag_the_command_explains_itself() -> TestResult {
        let root = project("noflag", CLEAN_SOURCE)?;
        let result = run(&root, false);
        assert!(result.is_err(), "the bare `check` must explain the flag");
        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
