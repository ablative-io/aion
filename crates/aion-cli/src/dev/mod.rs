//! `aion dev`: the instant authoring loop.
//!
//! Watches a workflow project's source and, on every save, rebuilds and
//! type-checks it (reusing the toolchain's external `gleam build` shell-out),
//! repackages it (reusing `aion-package`), and hot-loads the new content-hash
//! version into a running server (reusing the operator deploy RPC) — with no
//! engine restart. A run already in flight stays pinned on the immutable
//! version it started with (invariant #5); only fresh runs pick up the reload.

/// Command-line arguments for `aion dev`.
pub mod args;
/// The rebuild → repackage → hot-load pipeline run on every save.
pub mod pipeline;
/// The event-driven filesystem watch loop.
pub mod watch;

pub use args::DevArgs;

use anyhow::Result;
use serde_json::{Value, json};

use crate::deploy::DeployTarget;
use watch::WatchSession;

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
