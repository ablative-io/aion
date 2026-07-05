//! Shared handler layer over Engine.
//!
//! Module layout:
//! - `deploy` — operator deploy (load/list/route/unload) handlers.
//! - `workflows` — start/signal/query/cancel workflow operation handlers.
//! - `visibility` — list/count handlers and namespace filter scoping.
//! - `describe` — describe-workflow handler.
//! - `runs` — run-id resolution and terminal-status reads.
//! - `payload` — required-field and envelope encode/decode helpers.
//! - `error` — engine-error-to-wire-error mapping.

/// Operator deploy handlers shared by both transports.
pub mod deploy;
mod describe;
mod error;
mod payload;
mod runs;
mod visibility;
mod workflows;

#[cfg(test)]
mod test_support;

pub use describe::describe;
pub use visibility::{count, list};
pub use workflows::{cancel, pause, query, reopen, resume, signal, start, start_with_placement};
