//! Shared fixtures for the `aion new` scaffold gates: drive the real `aion`
//! binary, patch the emitted `aion_flow` requirement to the workspace SDK
//! checkout, and build/package the generated project from source.
//!
//! A missing `gleam` CLI FAILS these gates with an explicit error. It must
//! never be downgraded to a skip: a silently skipped gate is exactly how
//! unvalidated artifacts ship.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub type TestError = Box<dyn std::error::Error>;

/// Runs the real `aion` binary with `args` from `current_dir` and captures
/// the output.
pub fn run_cli(current_dir: &Path, args: &[&str]) -> Result<Output, TestError> {
    Ok(Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(args)
        .current_dir(current_dir)
        .output()?)
}

/// Asserts a successful exit and returns stdout parsed as JSON.
pub fn success_json(output: &Output) -> Result<serde_json::Value, TestError> {
    if output.status.code() != Some(0) {
        return Err(format!(
            "expected success, got {:?}; stdout: {} stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Scaffolds `<parent>/<name>` with `aion new <name> <extra..>` and returns
/// the project directory together with the printed JSON report.
pub fn scaffold_project(
    parent: &Path,
    name: &str,
    extra: &[&str],
) -> Result<(PathBuf, serde_json::Value), TestError> {
    let mut args = vec!["new", name];
    args.extend_from_slice(extra);
    let output = run_cli(parent, &args)?;
    let report = success_json(&output)?;
    Ok((parent.join(name), report))
}

/// Re-points the emitted hex `aion_flow` requirement at the workspace SDK
/// checkout, so the gate builds against the source that matches this engine.
/// Fails when the emitted requirement is not the published range — the
/// scaffold must reference hex, not a local path.
pub fn patch_aion_flow_to_workspace(project: &Path) -> Result<(), TestError> {
    let manifest_path = project.join("gleam.toml");
    let manifest = std::fs::read_to_string(&manifest_path)?;
    let published = "aion_flow = \">= 0.3.0 and < 0.4.0\"";
    if !manifest.contains(published) {
        return Err(format!(
            "emitted gleam.toml must require the published aion_flow range; got:\n{manifest}"
        )
        .into());
    }
    let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../gleam/aion_flow")
        .canonicalize()?;
    let patched = manifest.replace(
        published,
        &format!("aion_flow = {{ path = \"{}\" }}", sdk.display()),
    );
    std::fs::write(manifest_path, patched)?;
    Ok(())
}

/// Builds the generated Gleam project from source.
pub fn gleam_build(project: &Path) -> Result<(), TestError> {
    let status = Command::new("gleam")
        .arg("build")
        .current_dir(project)
        .status()
        .map_err(|error| {
            format!(
                "the scaffold gate requires the `gleam` CLI on PATH (failed to \
                 spawn `gleam build` in {}: {error}); this gate fails loudly by \
                 design — never reintroduce a skip",
                project.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "`gleam build` failed in {} with {status}: the scaffolded project must compile",
            project.display()
        )
        .into());
    }
    Ok(())
}

/// Packages the built project with the real `aion package` and returns the
/// archive path the descriptor declares.
pub fn package_project(project: &Path, name: &str) -> Result<PathBuf, TestError> {
    let output = run_cli(project, &["package", "."])?;
    let report = success_json(&output)?;
    let packaged = report["packages"]
        .as_array()
        .and_then(|packages| packages.first())
        .ok_or("aion package must report one packaged workflow")?;
    if packaged["workflow_type"] != name {
        return Err(format!(
            "packaged workflow type must be {name}; report: {report}"
        )
        .into());
    }
    let archive = project.join(format!("{name}.aion"));
    if !archive.is_file() {
        return Err(format!(
            "aion package did not produce the declared archive {}",
            archive.display()
        )
        .into());
    }
    Ok(archive)
}
