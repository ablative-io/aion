//! The `agent` scaffold for `aion new`: a durable agent loop
//! (scout -> act -> verify -> signal-gated human review) emitted as a
//! runnable `aion_flow` Gleam package plus its agent-step worker crate.
//!
//! The shape generalises the dogfooded stacked-dev family into configuration:
//! the three agent steps are worker-served activities parameterised by
//! prompts carried in the start input, and the human approval pause is a
//! durable `workflow.receive` raced against a caller-chosen deadline — not a
//! bespoke polling mechanism. The scaffold bundles no agent runtime; the
//! worker (`worker/`) is where a real driver plugs in, and it stays
//! worker-side (ADR-011). No step or review deadline is invented (ADR-003):
//! the per-step activities run unbounded until the worker answers, and the
//! review wait uses the required `review_timeout_ms` start-input field.
//!
//! This module owns only the agent template's file manifest; placeholder
//! substitution, name validation, target-directory checks, and emission live
//! in [`crate::new::scaffold`], shared with every other template.

use crate::new::template::ManifestFile;

/// The agent template's project files (shared files are prepended by the
/// template layer). The `src/{{name}}.gleam` workflow drives the loop; the
/// schemas, `workflow.toml`, and `README.md` complete a runnable package.
pub const AGENT_FILES: &[ManifestFile] = &[
    (
        "workflow.toml",
        include_str!("../../templates/agent/workflow.toml"),
    ),
    (
        "schemas/input.json",
        include_str!("../../templates/agent/schemas/input.json"),
    ),
    (
        "schemas/output.json",
        include_str!("../../templates/agent/schemas/output.json"),
    ),
    (
        "src/{{name}}.gleam",
        include_str!("../../templates/agent/project.gleam"),
    ),
    ("README.md", include_str!("../../templates/agent/README.md")),
];

/// The agent-step worker crate emitted with `--worker rust`. It serves the
/// three parameterised steps the workflow dispatches; the handler bodies are
/// trivial echoes a real agent driver replaces.
///
/// The on-disk manifest is `Cargo.toml.tmpl`, not `Cargo.toml`: cargo excludes
/// subdirectories containing a `Cargo.toml` when packaging this crate, which
/// would break `include_str!` on the published crate.
pub const AGENT_WORKER_FILES: &[ManifestFile] = &[
    (
        "worker/Cargo.toml",
        include_str!("../../templates/agent/worker/Cargo.toml.tmpl"),
    ),
    (
        "worker/src/main.rs",
        include_str!("../../templates/agent/worker/main.rs"),
    ),
];

/// The three parameterised agent steps the workflow dispatches to the worker.
pub const AGENT_ACTIVITIES: &[&str] = &["scout", "act", "verify"];
