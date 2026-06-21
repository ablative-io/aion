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
/// The session entry point that resolves the project and installs the watcher.
pub mod session;
/// The event-driven filesystem watch loop.
pub mod watch;

pub use args::DevArgs;
pub use session::run;
