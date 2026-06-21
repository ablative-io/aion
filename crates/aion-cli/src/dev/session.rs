//! The `aion dev` session entry point: resolve the project, install the
//! watcher, and block until the loop ends.

use anyhow::Result;
use serde_json::{Value, json};

use crate::deploy::DeployTarget;

use super::args::DevArgs;
use super::watch::{self, WatchSession};

/// Runs the `aion dev` loop against a running server.
///
/// This never returns under normal operation: it watches until the process is
/// interrupted (Ctrl-C / SIGTERM). The returned [`Value`] is the terminal
/// status document printed if the watch loop ends because its watcher was torn
/// down.
///
/// # Errors
///
/// Returns an error when the project path cannot be resolved or the filesystem
/// watcher cannot be installed. Per-save rebuild failures do not end the loop;
/// they are reported to stderr and watching continues.
pub async fn run(args: &DevArgs, target: DeployTarget) -> Result<Value> {
    let project_root = std::path::absolute(&args.path)?;
    let session = WatchSession {
        project_root: &project_root,
        gleam_path: &args.gleam_path,
        target: &target,
        debounce: args.debounce_ms,
    };
    watch::watch(&session).await?;
    Ok(json!({
        "dev": "stopped",
        "project_root": project_root.display().to_string(),
    }))
}
