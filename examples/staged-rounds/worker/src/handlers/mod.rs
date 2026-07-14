//! The three SHELL-node activity handler bodies: `provision_item`,
//! `merge_branches`, and the pure `fold_phase` boundary. The first two shell
//! to real `git` through [`crate::shell::Shell`]; phase folding is pure.
//!
//! Failure taxonomy (the dev-brief discipline, kept): a command that cannot
//! RUN (missing executable, dead working directory) is INFRASTRUCTURE
//! failure — a terminal `ActivityFailure`, because retrying a broken
//! environment cannot help. A merge CONFLICT is RECORDED DATA returned as a
//! successful activity result — never an error, so the conflict lands in
//! durable workflow history with the merge left in progress for the
//! remediator. Nothing is ever swallowed into a success: every conflict
//! rides back with its captured output.
//!
//! The bodies are plain synchronous `(&Shell, Input) -> Result<Output, _>`
//! (or pure `Input -> Result<Output, _>`) so the hermetic tests drive them
//! directly; `main.rs` adapts them onto the worker's async handler
//! signature.
//!
//! Module layout (declarations and re-exports only — logic lives in the
//! named children):
//! - [`provision`] — per-item worktree provisioning (reuse, don't reset).
//! - [`fold_phase`] — the pure phase-state fold (derive-and-check).
//! - [`merge`] — the continue-style integration merge.
//! - [`support`] — the shared shell/clipping helpers ([`clip`] is public
//!   API).

mod fold_phase;
mod merge;
mod provision;
mod support;

pub use fold_phase::fold_phase;
pub use merge::{MERGE_COMMIT_EMAIL, MERGE_COMMIT_NAME, merge_branches};
pub use provision::provision_item;
pub use support::clip;
