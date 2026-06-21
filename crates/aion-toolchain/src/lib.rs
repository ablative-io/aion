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
//! # Blocking
//!
//! [`compile_source`] and [`compile_project`] are synchronous and block on
//! `gleam build` and packaging, both of which can run for seconds. Async
//! callers MUST wrap them in a blocking task (for example
//! `tokio::task::spawn_blocking`).

/// Core compile/type-check/package loop over the external `gleam` binary.
pub mod compile;
/// Toolchain error taxonomy.
pub mod error;
/// Project-root validation and entry-module source resolution.
pub mod project;

pub use compile::{CompileRequest, CompiledWorkflow, compile_project, compile_source};
pub use error::ToolchainError;
