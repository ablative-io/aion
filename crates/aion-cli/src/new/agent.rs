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
//! The template is on the types-first path (ADR-014): the boundary types are
//! authored once in `src/{{name}}_io.gleam`, and the JSON codecs module plus
//! every `schemas/*.json` artifact ship pre-generated — byte-identical to
//! what `aion generate` reproduces from the types module (proven by the
//! scaffold gates), so a fresh scaffold builds without running the toolchain
//! and `aion generate . --check` passes clean. The template pins the
//! published hex `aion_flow` 0.4.x, which predates `activity.declare` /
//! `aion/manifest` and `workflow.entrypoint`, so the workflow keeps the
//! generated-marker `run` adapter and the activity declarations stay implicit
//! in the workflow body; once 0.5.x publishes, an authored
//! `src/{{name}}_activities.gleam` `manifest()` puts the activity wrappers,
//! the worker `main.rs`, and the wire-compat golden under `aion generate` too
//! (0.5.0-publish follow-up).
//!
//! This module owns only the agent template's file manifest; placeholder
//! substitution, name validation, target-directory checks, and emission live
//! in [`crate::new::scaffold`], shared with every other template.

use crate::new::template::ManifestFile;

/// The agent template's project files (shared files are prepended by the
/// template layer). The `src/{{name}}.gleam` workflow drives the loop over
/// the authored boundary types in `src/{{name}}_io.gleam`; the pre-generated
/// codecs module and schema artifacts, `workflow.toml`, and `README.md`
/// complete a runnable package.
pub const AGENT_FILES: &[ManifestFile] = &[
    (
        "workflow.toml",
        include_str!("../../templates/agent/workflow.toml"),
    ),
    // Authored source of truth: the boundary types (types-first, ADR-014).
    (
        "src/{{name}}_io.gleam",
        include_str!("../../templates/agent/project_io.gleam"),
    ),
    // Pre-generated artifacts, byte-identical to what `aion generate`
    // reproduces from the types module (proven by the scaffold gates), so
    // the fresh scaffold builds without running the toolchain.
    (
        "src/{{name}}_codecs.gleam",
        include_str!("../../templates/agent/project_codecs.gleam"),
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
        "schemas/step_input.json",
        include_str!("../../templates/agent/schemas/step_input.json"),
    ),
    (
        "schemas/step_output.json",
        include_str!("../../templates/agent/schemas/step_output.json"),
    ),
    (
        "schemas/decision.json",
        include_str!("../../templates/agent/schemas/decision.json"),
    ),
    (
        "schemas/review_signal.json",
        include_str!("../../templates/agent/schemas/review_signal.json"),
    ),
    (
        "schemas/agent_status.json",
        include_str!("../../templates/agent/schemas/agent_status.json"),
    ),
    (
        "src/{{name}}.gleam",
        include_str!("../../templates/agent/project.gleam"),
    ),
    ("README.md", include_str!("../../templates/agent/README.md")),
];

/// The agent-step worker crate emitted with `--worker rust`. It serves the
/// three parameterised steps the workflow dispatches (`RemoteRust` tier,
/// ADR-011 — the agent driver stays worker-side); its `StepInput`/`StepOutput`
/// structs mirror the boundary types declared in `src/{{name}}_io.gleam`, and
/// the handler bodies are trivial echoes a real agent driver replaces. Once
/// the 0.5.x SDK publishes and the template gains an authored activities
/// `manifest()`, this `main.rs` becomes an `aion generate` artifact (the
/// bodies move to a hand-written `handlers.rs`).
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
