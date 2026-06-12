//! From-scaffold build gates for `aion new`: every template's generated
//! project must compile with `gleam build` and package with the real
//! `aion package` binary, and the `--worker rust` crate must pass
//! `cargo check`. The hello-world template is proven end to end (server
//! boot, deploy, start, completion) in `new_hello_world_e2e.rs`.

mod common;

use std::path::Path;
use std::process::Command;

use common::TestError;

/// Scaffold → `gleam build` → `aion package` for one template.
fn build_and_package(name: &str, template: &str) -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let (project, report) =
        common::scaffold_project(temp_dir.path(), name, &["--template", template])?;
    assert_eq!(report["template"], template);
    assert_eq!(report["project"], name);
    let files = report["files"]
        .as_array()
        .ok_or("scaffold report must list files")?;
    assert!(
        files
            .iter()
            .any(|file| file == &format!("src/{name}.gleam")),
        "scaffold must emit the project module: {report}"
    );

    common::patch_aion_flow_to_workspace(&project)?;
    common::gleam_build(&project)?;
    common::package_project(&project, name)?;
    Ok(())
}

#[test]
fn hello_world_template_builds_and_packages() -> Result<(), TestError> {
    build_and_package("hello_build_gate", "hello-world")
}

#[test]
fn approval_flow_template_builds_and_packages() -> Result<(), TestError> {
    build_and_package("approval_build_gate", "approval-flow")
}

#[test]
fn saga_template_builds_and_packages() -> Result<(), TestError> {
    build_and_package("saga_build_gate", "saga")
}

/// The `--worker rust` crate must reference the published `aion-worker`
/// version and compile. The check builds against the workspace crate via a
/// `[patch.crates-io]` appended by the test only; the emitted manifest is
/// asserted to require the published version first.
#[test]
fn saga_worker_crate_passes_cargo_check() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let (project, report) = common::scaffold_project(
        temp_dir.path(),
        "saga_worker_gate",
        &["--template", "saga", "--worker", "rust"],
    )?;
    assert_eq!(report["worker"], "rust");

    let manifest_path = project.join("worker/Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)?;
    let published = format!("aion-worker = \"{}\"", env!("CARGO_PKG_VERSION"));
    assert!(
        manifest.contains(&published),
        "emitted worker manifest must require the published SDK ({published}); got:\n{manifest}"
    );

    let workspace_worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aion-worker")
        .canonicalize()?;
    let patched = format!(
        "{manifest}\n[patch.crates-io]\naion-worker = {{ path = \"{}\" }}\n",
        workspace_worker.display()
    );
    std::fs::write(&manifest_path, patched)?;

    let status = Command::new("cargo")
        .args(["check", "--quiet"])
        .current_dir(project.join("worker"))
        .status()
        .map_err(|error| format!("failed to spawn `cargo check`: {error}"))?;
    assert!(
        status.success(),
        "the scaffolded worker crate must compile; `cargo check` exited with {status}"
    );
    Ok(())
}

#[test]
fn new_refuses_non_empty_directory() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let target = temp_dir.path().join("occupied");
    std::fs::create_dir(&target)?;
    std::fs::write(target.join("keep.txt"), "existing work")?;

    let output = common::run_cli(temp_dir.path(), &["new", "occupied"])?;
    assert_eq!(output.status.code(), Some(1), "refusal must exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not empty"),
        "refusal must name the cause: {stderr}"
    );
    assert!(
        std::fs::read_to_string(target.join("keep.txt"))? == "existing work",
        "existing content must be untouched"
    );
    Ok(())
}

#[test]
fn new_rejects_invalid_names_with_the_rule() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let output = common::run_cli(temp_dir.path(), &["new", "Kebab-Case"])?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("snake_case"),
        "rejection must state the naming rule: {stderr}"
    );
    Ok(())
}

#[test]
fn new_refuses_worker_for_templates_without_activities() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let output = common::run_cli(
        temp_dir.path(),
        &["new", "no_worker", "--template", "hello-world", "--worker", "rust"],
    )?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no activities"),
        "refusal must explain there is nothing to serve: {stderr}"
    );
    assert!(
        !temp_dir.path().join("no_worker").exists(),
        "a refused scaffold must write nothing"
    );
    Ok(())
}
