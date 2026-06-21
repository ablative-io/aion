//! The rebuild → repackage → hot-load pipeline run on every save.
//!
//! Each stage reuses existing infrastructure rather than reinventing it:
//!
//! * rebuild / type-check — [`aion_toolchain::build_project`], the same
//!   external `gleam build` shell-out (with captured diagnostics) the
//!   server-side authoring loop uses;
//! * repackage — [`aion_package::package_project`], the same packaging the
//!   `aion package` command drives, producing the content-hash-versioned
//!   `.aion`;
//! * hot-load — the operator deploy path ([`crate::deploy::deploy`]), the same
//!   gRPC `LoadPackage` call `aion deploy` makes, so the new content-hash
//!   version is registered and routed for fresh runs while every run already
//!   in flight stays pinned on its immutable started-on version.
//!
//! There is no engine restart on this path: the running server hot-loads the
//! new module under its content-hash namespace (invariant #5).

use std::path::{Path, PathBuf};

use aion_package::{PackageOptions, package_project};
use aion_toolchain::build_project;
use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::deploy::DeployTarget;

/// Outcome of one successful rebuild → repackage → hot-load cycle.
#[derive(Clone, Debug)]
pub struct ReloadOutcome {
    /// Logical workflow type that was rebuilt and loaded.
    pub workflow_type: String,
    /// Content hash of the freshly loaded package version.
    pub content_hash: String,
    /// Whether this load registered a new version (false on an idempotent
    /// re-load of an unchanged content hash).
    pub freshly_loaded: bool,
    /// Whether this load re-pointed the type's route at the new version.
    pub route_changed: bool,
    /// On-disk path of the packaged `.aion` archive that was loaded.
    pub archive_path: PathBuf,
}

/// Rebuilds, repackages, and hot-loads the single workflow at `project_root`.
///
/// The project must declare exactly one workflow — the instant authoring loop
/// targets one workflow at a time, so an ambiguous multi-workflow project is a
/// loud error rather than a silent first-wins choice.
///
/// # Errors
///
/// Returns the rebuild diagnostics when `gleam build` fails, a packaging error
/// when the built project cannot be assembled, an ambiguity error when the
/// project declares anything other than one workflow, and the deploy transport
/// or wire error when the running server rejects the hot-load.
pub async fn rebuild_repackage_reload(
    project_root: &Path,
    gleam_path: &Path,
    target: &DeployTarget,
) -> Result<ReloadOutcome> {
    build_project(project_root, gleam_path)
        .with_context(|| format!("failed to rebuild project at {}", project_root.display()))?;

    let report = package_project(project_root, &PackageOptions::default())
        .with_context(|| format!("failed to package project at {}", project_root.display()))?;
    let packaged = match report.packages.len() {
        1 => report
            .packages
            .into_iter()
            .next()
            .context("packaged workflow vanished after length check")?,
        count => bail!(
            "`aion dev` watches a single workflow, but the project at {} packaged {count} workflows; \
             split them into separate projects or run the server-side deploy surface",
            project_root.display()
        ),
    };
    let archive_path = packaged.output_path.clone();

    // Hot-load over the very same operator deploy RPC `aion deploy` uses, so the
    // running engine registers the new content-hash version and re-points
    // routing for fresh runs — no engine restart, no second load path.
    let response = crate::deploy::deploy(target, &archive_path)
        .await
        .context("failed to hot-load the rebuilt package into the running server")?;

    reload_outcome_from_response(&response, archive_path)
}

/// Extracts the structured reload outcome from the deploy response document.
fn reload_outcome_from_response(response: &Value, archive_path: PathBuf) -> Result<ReloadOutcome> {
    let string_field = |key: &str| -> Result<String> {
        response
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("deploy response missing string field `{key}`"))
    };
    let bool_field = |key: &str| -> Result<bool> {
        response
            .get(key)
            .and_then(Value::as_bool)
            .with_context(|| format!("deploy response missing boolean field `{key}`"))
    };
    Ok(ReloadOutcome {
        workflow_type: string_field("workflow_type")?,
        content_hash: string_field("content_hash")?,
        freshly_loaded: bool_field("freshly_loaded")?,
        route_changed: bool_field("route_changed")?,
        archive_path,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::reload_outcome_from_response;

    #[test]
    fn reload_outcome_reads_the_deploy_response_document() -> anyhow::Result<()> {
        let response = json!({
            "workflow_type": "order",
            "content_hash": "abc123",
            "deployed_entry_module": "order$abc123",
            "entry_function": "run",
            "freshly_loaded": true,
            "route_changed": true,
        });

        let outcome = reload_outcome_from_response(&response, PathBuf::from("dist/order.aion"))?;

        assert_eq!(outcome.workflow_type, "order");
        assert_eq!(outcome.content_hash, "abc123");
        assert!(outcome.freshly_loaded);
        assert!(outcome.route_changed);
        assert_eq!(outcome.archive_path, PathBuf::from("dist/order.aion"));
        Ok(())
    }

    #[test]
    fn reload_outcome_rejects_a_response_missing_a_field() {
        let response = json!({ "workflow_type": "order" });
        assert!(reload_outcome_from_response(&response, PathBuf::from("x.aion")).is_err());
    }
}
