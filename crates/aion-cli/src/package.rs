//! Local `package` subcommand: a thin shell over [`aion_package::package_project`].

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use aion_package::{ExcludedModule, PackageOptions, ProjectReport, package_project};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::output::to_value;

/// JSON document printed on stdout after a successful `package` run.
#[derive(Serialize)]
struct PackageOutput<'a> {
    packages: Vec<PackagedOutput>,
    excluded: &'a [ExcludedModule],
}

/// One packaged workflow in the `package` result document.
#[derive(Serialize)]
struct PackagedOutput {
    workflow_type: String,
    output: String,
    version: String,
    deployed_name: String,
    modules: usize,
}

/// Runs the `package` subcommand: optionally builds the Gleam project, then
/// packages every workflow its `workflow.toml` declares.
///
/// `out` is resolved against the current directory before it reaches the
/// library, which would otherwise resolve it against the project root.
pub(crate) fn run(path: &Path, out: Option<&Path>, build: bool) -> Result<Value> {
    if build {
        run_gleam_build(path)?;
    }
    let options = PackageOptions {
        output_override: out.map(absolute_out).transpose()?,
    };
    let report = package_project(path, &options)
        .with_context(|| format!("failed to package workflow project at {}", path.display()))?;
    to_value(report_output(&report))
}

/// Resolves a `--out` value against the invoker's current directory.
fn absolute_out(out: &Path) -> Result<PathBuf> {
    std::path::absolute(out)
        .with_context(|| format!("failed to resolve --out path {}", out.display()))
}

/// Spawns `gleam build` in the project directory with inherited stdio, so the
/// user sees compiler output on stderr. Process spawning lives only in this
/// CLI layer; the packaging library never builds.
fn run_gleam_build(path: &Path) -> Result<()> {
    let status = Command::new("gleam")
        .arg("build")
        .current_dir(path)
        .status()
        .with_context(|| format!("failed to run `gleam build` in {}", path.display()))?;
    if !status.success() {
        bail!("`gleam build` failed in {} with {status}", path.display());
    }
    Ok(())
}

/// Maps the library report to the printed JSON document.
fn report_output(report: &ProjectReport) -> PackageOutput<'_> {
    PackageOutput {
        packages: report
            .packages
            .iter()
            .map(|packaged| PackagedOutput {
                workflow_type: packaged.workflow_type.clone(),
                output: packaged.output_path.display().to_string(),
                version: packaged.version.content_hash.to_string(),
                deployed_name: packaged.package.deployed_entry_module(),
                modules: packaged.package.beams().len(),
            })
            .collect(),
        excluded: &report.excluded,
    }
}
