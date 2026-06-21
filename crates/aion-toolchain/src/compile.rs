//! Core authoring compile loop: stage an isolated working copy of the project
//! template, write the submitted source into it, run `gleam build` there, and
//! package on success.
//!
//! This module is the clean generalization of the `gleam build` shell-out
//! already used by the local `aion package` command: it spawns the external
//! `gleam` binary (no embedded compiler), but captures the compiler's output
//! instead of inheriting stdio so the diagnostics can travel back over the
//! wire, and it accepts a configurable binary path so the server can be
//! pointed at an operator-provided `gleam`.
//!
//! # Per-submission isolation
//!
//! The configured project root is treated as a **read-only template**. Each
//! call stages its own throwaway working copy (a [`Workspace`]) and writes,
//! builds, and packages entirely inside that copy, so concurrent submissions
//! are fully isolated — no shared entry-file, no shared `build/` directory, no
//! shared `.aion` output, no global lock, and no pool-size cap (ADR-001). The
//! working copy is removed when the [`Workspace`] drops, on every path.
//!
//! The toolchain never rewrites the author's source: it writes the submitted
//! bytes verbatim into the entry module's file and packages the build output.
//! The determinism boundary (invariant 2) is the author's responsibility and
//! stays untouched here.

use std::path::{Path, PathBuf};
use std::process::Command;

use aion_package::{Package, PackageOptions, WorkflowVersion, package_project};

use crate::error::ToolchainError;
use crate::project;
use crate::workspace::Workspace;

/// A request to compile, type-check, and package submitted Gleam source.
pub struct CompileRequest<'a> {
    /// The built Gleam workflow project **template** the submission is built
    /// against. It must contain `gleam.toml`, `workflow.toml`, the `aion_flow`
    /// dependency, and `schemas/` — exactly as `aion new` produces one. It is
    /// read-only at request time: the toolchain copies it into a fresh
    /// per-submission workspace and never writes to or builds in it.
    pub template_root: &'a Path,
    /// Path to the external `gleam` binary the toolchain spawns. There is no
    /// default: the caller supplies it (the server resolves it from the
    /// operator-configured `[authoring].gleam_path`).
    pub gleam_path: &'a Path,
    /// The submitted Gleam source written verbatim to the workspace copy's
    /// single entry-module source file before building.
    pub source: &'a str,
}

/// A compiled, type-checked, and packaged workflow.
#[derive(Clone, Debug)]
pub struct CompiledWorkflow {
    /// The verified `.aion` package, re-loaded from disk after writing.
    pub package: Package,
    /// The canonical version record of the verified package.
    pub version: WorkflowVersion,
    /// The workflow type (the manifest entry module).
    pub workflow_type: String,
    /// The absolute path of the written `.aion` archive.
    pub output_path: PathBuf,
}

/// Compiles, type-checks, and packages submitted Gleam source against a
/// read-only project template, in a fresh per-submission workspace.
///
/// Validates the template is a usable single-workflow Gleam project, stages an
/// isolated working copy of it (a [`Workspace`] — a sibling temp dir that is
/// removed on drop), writes `request.source` into the working copy's single
/// entry-module source file, runs `gleam build` in the working copy (capturing
/// its output), and — only on a zero exit — packages the working copy into a
/// verified `.aion`. The template is never written to or built in, so
/// concurrent submissions are fully isolated.
///
/// This is synchronous and blocks on `gleam build` and packaging, both of
/// which can run for seconds. Async callers MUST wrap it in a blocking task
/// (for example `tokio::task::spawn_blocking`).
///
/// # Errors
///
/// Returns [`ToolchainError::InvalidProject`] when the template is not a usable
/// single-workflow Gleam project, the entry module name is unsafe, or the
/// template has no parent directory to host the workspace,
/// [`ToolchainError::Io`] when the working copy cannot be staged or the source
/// cannot be written, [`ToolchainError::GleamSpawn`] when the `gleam` binary
/// cannot be spawned, [`ToolchainError::TypeCheck`] (carrying the verbatim
/// compiler diagnostics) when the build exits non-zero, and
/// [`ToolchainError::Packaging`] when the built project cannot be assembled
/// into a verified archive.
pub fn compile_source(request: &CompileRequest<'_>) -> Result<CompiledWorkflow, ToolchainError> {
    // Validate the template up front so a misconfigured project root fails
    // before the cost of staging a working copy.
    project::validate_project_root(request.template_root)?;
    let entry_module = project::single_entry_module(request.template_root)?;

    // Every submission gets its own isolated working copy; the template is
    // never touched. The workspace (and the captured source within it) is
    // removed when `workspace` drops at the end of this function — on the
    // success path and on every `?` early return alike.
    let workspace = Workspace::stage(request.template_root)?;
    let workspace_root = workspace.root();

    let source_path = project::entry_module_source_path(workspace_root, &entry_module)?;
    project::write_entry_source(&source_path, request.source)?;
    compile_built_project(workspace_root, request.gleam_path)
}

/// Runs `gleam build` against `project_root` then packages it.
fn compile_built_project(
    project_root: &Path,
    gleam_path: &Path,
) -> Result<CompiledWorkflow, ToolchainError> {
    build_project(project_root, gleam_path)?;
    package_built_project(project_root)
}

/// Compiles and type-checks an on-disk Gleam workflow project in place by
/// spawning the external `gleam` binary against `project_root`, capturing its
/// diagnostics instead of inheriting stdio.
///
/// This is the single shell-out the toolchain owns: [`compile_source`] calls
/// it against a per-submission workspace copy, and the local `aion dev` watch
/// loop calls it directly against the author's project on disk. Neither path
/// reinvents the `gleam build` invocation or its diagnostic capture.
///
/// A non-zero exit is a [`ToolchainError::TypeCheck`] carrying the verbatim
/// compiler output (stderr, with any stdout appended): Gleam writes errors to
/// stderr, but context may split across both streams, so both are captured.
///
/// This is synchronous and blocks on `gleam build`, which can run for seconds;
/// async callers MUST wrap it in a blocking task.
///
/// # Errors
///
/// Returns [`ToolchainError::GleamSpawn`] when the `gleam` binary at
/// `gleam_path` cannot be spawned, and [`ToolchainError::TypeCheck`] (carrying
/// the verbatim compiler diagnostics) when the build exits non-zero.
pub fn build_project(project_root: &Path, gleam_path: &Path) -> Result<(), ToolchainError> {
    let output = Command::new(gleam_path)
        .arg("build")
        .current_dir(project_root)
        .output()
        .map_err(|source| ToolchainError::GleamSpawn {
            gleam_path: gleam_path.to_path_buf(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ToolchainError::TypeCheck {
        diagnostics: combine_diagnostics(&output.stderr, &output.stdout),
    })
}

/// Joins captured stderr and stdout into the inline diagnostics string.
///
/// Stderr leads (Gleam's errors land there); stdout is appended only when it
/// carries content, separated by a blank line.
fn combine_diagnostics(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let stderr_trimmed = stderr.trim_end();
    let stdout_trimmed = stdout.trim_end();
    match (stderr_trimmed.is_empty(), stdout_trimmed.is_empty()) {
        (false, false) => format!("{stderr_trimmed}\n\n{stdout_trimmed}"),
        (false, true) => stderr_trimmed.to_owned(),
        (true, false) => stdout_trimmed.to_owned(),
        (true, true) => {
            "gleam build failed with no diagnostic output on stderr or stdout".to_owned()
        }
    }
}

/// Packages the built project into a verified single-workflow `.aion`.
fn package_built_project(project_root: &Path) -> Result<CompiledWorkflow, ToolchainError> {
    let report = package_project(project_root, &PackageOptions::default())?;
    let mut built = report.packages;
    let packaged = match built.len() {
        1 => built.remove(0),
        count => {
            return Err(ToolchainError::InvalidProject {
                message: format!(
                    "authoring project packaged {count} workflows; source submission requires exactly one"
                ),
            });
        }
    };
    Ok(CompiledWorkflow {
        workflow_type: packaged.workflow_type,
        output_path: packaged.output_path,
        version: packaged.version,
        package: packaged.package,
    })
}

#[cfg(test)]
mod tests {
    use super::combine_diagnostics;

    #[test]
    fn diagnostics_prefer_stderr_and_append_stdout() {
        assert_eq!(combine_diagnostics(b"type error\n", b""), "type error");
        assert_eq!(combine_diagnostics(b"", b"compiling\n"), "compiling");
        assert_eq!(
            combine_diagnostics(b"type error\n", b"compiling demo\n"),
            "type error\n\ncompiling demo"
        );
        assert_eq!(
            combine_diagnostics(b"", b""),
            "gleam build failed with no diagnostic output on stderr or stdout"
        );
    }
}
