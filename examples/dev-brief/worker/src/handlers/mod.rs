//! The five SHELL activity handler bodies: `provision_workspace`,
//! `run_gates`, `reset_workspace`, `verify_gates`, `cleanup_workspace`. Each
//! shells to real `git` and the brief's configured gate commands through
//! [`crate::shell::Shell`].
//!
//! Failure taxonomy (the remediation/pipeline-run discipline, kept): a
//! command that cannot RUN (missing executable, dead working directory) is
//! INFRASTRUCTURE failure — a terminal `ActivityFailure`, because retrying
//! a broken environment cannot help. A gate command whose non-zero exit is a
//! CONTRACT verdict (a red clippy, a failing suite) is RECORDED DATA returned
//! as a successful activity result — never an error, so the exit status lands
//! in durable workflow history. Nothing is ever swallowed into a success:
//! every red command rides back with its captured output.
//!
//! The bodies are plain synchronous `(&Shell, Input) -> Result<Output, _>` so
//! the hermetic tests drive them directly; `main.rs` adapts them onto the
//! worker's async handler signature.
//!
//! Module layout (declarations and re-exports only — logic lives in the
//! named children):
//! - [`workspace`] — `provision` and `cleanup` (worktree lifecycle).
//! - [`gates`] — the shared gate-battery core (`{base_commit}` substitution,
//!   resolved-argv recording) and `run_gates`.
//! - [`reset`] — the post-review mechanical restore.
//! - [`verify`] — the post-accept verification stage and its untruncated log.
//! - [`support`] — the shared shell/clipping helpers ([`clip`] is public API).

mod gates;
mod reset;
mod support;
mod verify;
mod workspace;

pub use gates::run_gates;
pub use reset::reset;
pub use support::clip;
pub use verify::verify_gates;
pub use workspace::{cleanup, provision};
