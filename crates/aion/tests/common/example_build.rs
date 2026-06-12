//! From-source `.aion` archive builds for the example/fixture e2e gates.
//!
//! These gates previously loaded prebuilt archives and *skipped* when they
//! were missing. Archives are gitignored build artifacts, so the suite went
//! green against stale local builds containing an SDK generation no
//! committed source produced — the 0.2.0 release-integrity failure. Every
//! gate now rebuilds its archive from the committed Gleam source on each
//! run: `gleam build` followed by [`aion_package::package_project`].
//!
//! A missing `gleam` CLI FAILS the gate with an explicit error. It must
//! never be downgraded to a skip: a silently skipped gate is exactly how
//! unvalidated artifacts shipped.

use std::path::{Path, PathBuf};
use std::process::Command;

use aion_package::{Package, PackageOptions, ProjectReport, package_project};

/// Build the Gleam project at `<repo root>/<relative>` from source and
/// package every workflow its `workflow.toml` declares.
///
/// Concurrent test binaries building the same project are serialized with an
/// advisory file lock under `target/example-build-locks/`, so parallel
/// `cargo test` binaries never race one project's `build/` directory or its
/// archive outputs. Distinct projects build concurrently.
///
/// # Errors
///
/// Fails when the `gleam` CLI cannot be spawned (not installed / not on
/// PATH), when `gleam build` exits non-zero, or when packaging fails. There
/// is deliberately no skip path.
pub fn build_project(relative: &str) -> Result<ProjectReport, Box<dyn std::error::Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let project_root = repo_root.join(relative);
    let lock_dir = repo_root.join("target/example-build-locks");
    std::fs::create_dir_all(&lock_dir)?;
    let lock_path = lock_dir.join(format!("{}.lock", relative.replace(['/', '\\'], "-")));
    let lock_file = std::fs::File::create(&lock_path)?;
    // Fully qualified: `fs4::FileExt::lock` (advisory exclusive flock,
    // released on close or process death) — std's inherent `File::lock`
    // would shadow the trait method and is above the workspace MSRV.
    fs4::FileExt::lock(&lock_file).map_err(|error| {
        format!(
            "failed to take the build lock {}: {error}",
            lock_path.display()
        )
    })?;
    // The advisory lock is released when `lock_file` drops (closes), error
    // path included.
    build_and_package(&project_root)
}

/// Build the project at `<repo root>/<relative>` from source and return the
/// verified package for `workflow_type`.
///
/// # Errors
///
/// Fails when the build or packaging fails, or when the project does not
/// declare `workflow_type`.
pub fn built_package(
    relative: &str,
    workflow_type: &str,
) -> Result<Package, Box<dyn std::error::Error>> {
    let report = build_project(relative)?;
    let declared: Vec<&str> = report
        .packages
        .iter()
        .map(|packaged| packaged.workflow_type.as_str())
        .collect();
    report
        .packages
        .iter()
        .find(|packaged| packaged.workflow_type == workflow_type)
        .map(|packaged| packaged.package.clone())
        .ok_or_else(|| {
            format!("{relative} does not declare workflow type {workflow_type}: {declared:?}")
                .into()
        })
}

fn build_and_package(project_root: &Path) -> Result<ProjectReport, Box<dyn std::error::Error>> {
    let status = Command::new("gleam")
        .arg("build")
        .current_dir(project_root)
        .status()
        .map_err(|error| {
            format!(
                "the from-source archive gate requires the `gleam` CLI on PATH \
                 (failed to spawn `gleam build` in {}: {error}). This gate fails \
                 loudly by design — never reintroduce a skip: stale prebuilt \
                 archives are how unvalidated SDK binaries shipped",
                project_root.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "`gleam build` failed in {} with {status}",
            project_root.display()
        )
        .into());
    }
    Ok(package_project(project_root, &PackageOptions::default())?)
}
