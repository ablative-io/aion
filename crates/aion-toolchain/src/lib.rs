//! Server-side Gleam authoring toolchain.
//!
//! This crate turns a running Aion engine into a Gleam authoring loop: it
//! takes submitted workflow source, compiles and type-checks it, and — on
//! success — packages a verified `.aion` archive ready to hot-load.
//!
//! # No embedded compiler (CN7 / C12 / C15)
//!
//! The toolchain embeds **no** Gleam compiler. It only ever spawns the
//! external `gleam` binary (the caller supplies its path) and captures the
//! compiler's output. Its entire dependency set is [`aion_package`] (the
//! `.aion` format) plus `thiserror`; there is no Gleam compiler crate and no
//! `beamr` in the tree. The `no_gleam_compiler_in_dependency_tree` integration
//! test asserts this guarantee mechanically. Without the toolchain there is no
//! way to compile Gleam on the server, which is exactly why the server-side
//! authoring endpoints are gated on an explicit `gleam_path`.
//!
//! # Per-submission isolation
//!
//! [`compile_source`] treats the configured project root as a **read-only
//! template**: each call stages its own throwaway working copy (a
//! [`workspace::Workspace`]), writes and builds entirely within it, and removes
//! it on drop. Concurrent submissions never share an entry-file, a `build/`
//! directory, or a `.aion` output — no global lock and no pool-size cap.
//!
//! # Blocking
//!
//! [`compile_source`] is synchronous and blocks on staging the working copy,
//! `gleam build`, and packaging, all of which can run for seconds. Async
//! callers MUST wrap it in a blocking task (for example
//! `tokio::task::spawn_blocking`).

/// Core compile/type-check/package loop over the external `gleam` binary.
pub mod compile;
/// Toolchain error taxonomy.
pub mod error;
/// Project-root validation and entry-module source resolution.
pub mod project;
/// Per-submission isolated build workspace (RAII working copy of the template).
pub mod workspace;

pub use compile::{
    CompileRequest, CompiledWorkflow, build_project, compile_source, compile_source_for_entry,
};
pub use error::ToolchainError;
pub use workspace::Workspace;
