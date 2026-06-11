//! Shared handler layer over Engine.
//!
//! Module layout:
//! - `workflows` — start/signal/query/cancel workflow operation handlers.
//! - `visibility` — list/count handlers and namespace filter scoping.
//! - `describe` — describe-workflow handler.
//! - `runs` — run-id resolution and terminal-status reads.
//! - `payload` — required-field and envelope encode/decode helpers.
//! - `error` — engine-error-to-wire-error mapping.

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
pub use workflows::{cancel, query, signal, start};
