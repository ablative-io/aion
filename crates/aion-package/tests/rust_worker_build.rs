//! Compile gate for the generated `RemoteRust` worker (`worker/src/main.rs`).
//!
//! The Rust worker emitter (`codegen::activity_worker_rust`) is exercised only
//! by string-shape unit tests; no example or test ever feeds its output to
//! `rustc` against the real `aion-worker` SDK. A drift in that SDK's public
//! surface — a renamed `WorkerConfig` builder method, a changed
//! `register_activity` bound, a moved `HandlerFuture` — would therefore ship a
//! generated worker that cannot build, with green CI.
//!
//! This test closes that gap end to end: it builds a realistic source project
//! in a temp dir, drives the `aion_package` library generators
//! ([`codegen_project`], [`generate_codecs`], [`generate_activities`]) to emit
//! the do-not-edit `worker/src/main.rs`, wraps it in a compilable crate with a
//! `path` dependency on the in-tree `aion-worker`, supplies a hand-written
//! `handlers.rs` whose stub signatures match what `register_activity` requires,
//! and runs a nested `cargo build`. A non-zero build is a real generator/SDK
//! mismatch and fails this test, surfacing `cargo`'s own stderr.
//!
//! The schemas deliberately exercise the emitter's interesting paths: an
//! optional field (`Option<T>` + `#[serde(default, skip_serializing_if = …)]`),
//! a Rust-keyword field name (`match` → `r#match`), a string `enum`, a nested
//! record, and a list (`Vec<T>`). Note that the Gleam-facing schema layer
//! rejects any property whose name is a *Gleam* reserved word (so `type` can
//! never reach the Rust emitter), but `match` is a Rust keyword that is a valid
//! Gleam label, so it is the honest way to drive the `r#`-escaping path through
//! the real pipeline.
//!
//! The nested build pulls the worker's full dependency tree (tokio, tonic) and
//! is slow on a cold cache — that is the point, and it is acceptable. The build
//! runs in an isolated `CARGO_TARGET_DIR` so it never pollutes the workspace
//! target directory, and every temp directory drops at the end of the test.

use std::path::{Path, PathBuf};
use std::process::Command;

use aion_package::{
    ActivityDeclaration, CodegenMode, Tier, codegen_project, generate_activities, generate_codecs,
};

type TestError = Box<dyn std::error::Error>;

/// `gleam.toml` naming the generated module prefix (`src/demo_*.gleam`).
const GLEAM_TOML: &str = "name = \"demo\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n";

/// `workflow.toml` referencing the two activity value schemas. `codegen_project`
/// validates this descriptor before emitting `src/demo_io.gleam`.
const WORKFLOW_TOML: &str = r#"[[workflow]]
entry_module = "demo"
entry_function = "run"
timeout_seconds = 30
input_schema = "schemas/charge_input.json"
output_schema = "schemas/receipt.json"
activities = ["charge_payment"]
"#;

/// The activity input type (`ChargeInput`). Drives every interesting emitter
/// path: a required scalar, an optional scalar (`Option<String>`), a list
/// (`Vec<String>`), a Rust-keyword field name (`match` → `r#match`), a nested
/// record (`ChargeInputCustomer`), and a string enum (`ChargeInputKind`).
const CHARGE_INPUT_SCHEMA: &[u8] = br#"{
  "type": "object",
  "required": ["order_id", "tags", "match", "customer", "kind"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "note": { "type": "string" },
    "tags": { "type": "array", "items": { "type": "string" } },
    "match": { "type": "string" },
    "customer": {
      "type": "object",
      "required": ["customer_id"],
      "additionalProperties": false,
      "properties": {
        "customer_id": { "type": "string" },
        "vip": { "type": "boolean" }
      }
    },
    "kind": { "type": "string", "enum": ["standard", "express"] }
  }
}"#;

/// The activity output type (`Receipt`): a single required scalar field, enough
/// for the handler stub to construct a value with no imports.
const RECEIPT_SCHEMA: &[u8] = br#"{
  "type": "object",
  "required": ["payment_id"],
  "additionalProperties": false,
  "properties": { "payment_id": { "type": "string" } }
}"#;

/// Absolute path to the `aion-worker` crate, for the worker's `path` dependency.
fn aion_worker_dir() -> Result<PathBuf, TestError> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../aion-worker")
        .canonicalize()?)
}

/// Writes `relative` under `root`, creating parent directories first.
fn write_file(root: &Path, relative: &str, contents: &[u8]) -> Result<(), TestError> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Lays down the minimal source project the generators read: a `gleam.toml`, a
/// `workflow.toml`, and the two value schemas under `schemas/`.
fn stage_source_project(root: &Path) -> Result<(), TestError> {
    write_file(root, "gleam.toml", GLEAM_TOML.as_bytes())?;
    write_file(root, "workflow.toml", WORKFLOW_TOML.as_bytes())?;
    write_file(root, "schemas/charge_input.json", CHARGE_INPUT_SCHEMA)?;
    write_file(root, "schemas/receipt.json", RECEIPT_SCHEMA)?;
    // `codegen_project` writes `src/demo_io.gleam` directly and does not create
    // the `src/` directory itself, so it must already exist.
    std::fs::create_dir_all(root.join("src"))?;
    Ok(())
}

/// Runs the library generators that emit `worker/src/main.rs`, in the order the
/// CLI uses: schema-driven codecs first, then the declaration-driven plumbing.
fn generate_worker(root: &Path) -> Result<(), TestError> {
    // The codecs and the io module are Gleam artifacts the CLI always emits;
    // running them here proves the whole generation path the worker rides on,
    // not just the Rust emitter in isolation.
    codegen_project(root, CodegenMode::Write)?;
    generate_codecs(root, CodegenMode::Write)?;

    let declarations = vec![ActivityDeclaration {
        name: "charge_payment".to_owned(),
        tier: Tier::RemoteRust,
        input_type: "ChargeInput".to_owned(),
        output_type: "Receipt".to_owned(),
    }];
    generate_activities(root, &declarations, CodegenMode::Write)?;

    let main_rs = root.join("worker/src/main.rs");
    if !main_rs.is_file() {
        return Err(format!(
            "generation did not emit the RemoteRust worker at {}",
            main_rs.display()
        )
        .into());
    }
    Ok(())
}

/// Writes the `worker/Cargo.toml` that wraps the generated `main.rs` into a
/// buildable crate: a detached (`[workspace]`) package on the workspace edition,
/// a `path` dependency on the real `aion-worker`, and the `tokio`/`serde`
/// features the generated `#[tokio::main]` and `#[derive(Serialize, …)]` need.
fn write_worker_cargo_toml(root: &Path, worker_dir: &Path) -> Result<(), TestError> {
    let manifest = format!(
        r#"[package]
name = "generated-rust-worker"
version = "0.0.0"
edition = "2024"
publish = false

# Detach from the aion workspace: this crate stands in for the worker a
# deployment would build against the SDK, so it resolves its own dep tree.
[workspace]

[[bin]]
name = "generated-rust-worker"
path = "src/main.rs"

[dependencies]
aion-worker = {{ path = "{worker}" }}
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
serde = {{ version = "1", features = ["derive"] }}
"#,
        worker = worker_dir.display(),
    );
    write_file(root, "worker/Cargo.toml", manifest.as_bytes())?;
    Ok(())
}

/// Writes the hand-authored `worker/src/handlers.rs` the generated `main.rs`
/// dispatches to. The stub matches the exact bound `register_activity` requires
/// — `Fn(Input, &ActivityContext) -> HandlerFuture<'_, Output>` — and returns a
/// ready future of a zero-valued `Receipt`. It type-checks the generated
/// registration call; it is never executed.
fn write_handlers(root: &Path) -> Result<(), TestError> {
    const HANDLERS: &str = "//! Hand-written activity bodies for the generated worker (test stub).\n\
        \n\
        use aion_worker::{ActivityContext, HandlerFuture};\n\
        \n\
        use crate::{ChargeInput, Receipt};\n\
        \n\
        /// Trivial stub satisfying the generated `register_activity` bound. The\n\
        /// input is ignored and a zero-valued receipt is returned; the body is\n\
        /// only ever type-checked, never run.\n\
        pub fn charge_payment(input: ChargeInput, _context: &ActivityContext) -> HandlerFuture<'_, Receipt> {\n    \
        let _ = input;\n    \
        Box::pin(async move {\n        \
        Ok(Receipt {\n            \
        payment_id: String::new(),\n        \
        })\n    \
        })\n\
        }\n";
    write_file(root, "worker/src/handlers.rs", HANDLERS.as_bytes())?;
    Ok(())
}

/// Builds the assembled worker crate with an isolated target directory, so the
/// nested build never touches the workspace `target/`.
fn build_worker(root: &Path, target_dir: &Path) -> Result<(bool, String), TestError> {
    let output = Command::new(env!("CARGO"))
        .current_dir(root.join("worker"))
        .arg("build")
        .env("CARGO_TARGET_DIR", target_dir)
        .output()
        .map_err(|error| {
            format!("failed to spawn `cargo build` for the generated worker: {error}")
        })?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), combined))
}

/// Generates a `RemoteRust` worker and compiles it against the real
/// `aion-worker` crate, failing if it does not build.
#[test]
fn generated_rust_worker_compiles_against_real_aion_worker() -> Result<(), TestError> {
    // `cargo` is provided by the test runner via `CARGO`; if it is somehow
    // unset the gate cannot run, so it skips loudly rather than panicking.
    if option_env!("CARGO").is_none() {
        eprintln!(
            "SKIP generated_rust_worker_compiles_against_real_aion_worker: \
             `CARGO` is not set, cannot run the nested build"
        );
        return Ok(());
    }

    let worker_dir = aion_worker_dir()?;
    let project = tempfile::tempdir()?;
    let root = project.path();

    stage_source_project(root)?;
    generate_worker(root)?;
    write_worker_cargo_toml(root, &worker_dir)?;
    write_handlers(root)?;

    // Isolated target dir in its own temp dir, dropped with the test.
    let target = tempfile::tempdir()?;
    let (built, output) = build_worker(root, target.path())?;
    if !built {
        return Err(format!(
            "the generated RemoteRust worker failed to compile against the real \
             `aion-worker` crate — this is a generator/SDK drift, not a test bug. \
             Fix the emitter in crates/aion-package/src/codegen/activity_worker_rust.rs, \
             never the generated file.\n\ncargo output:\n{output}"
        )
        .into());
    }

    // Prove the build actually ran (rather than no-opping on a stale artifact):
    // the binary the manifest names must exist in the isolated target dir.
    let binary = target.path().join("debug/generated-rust-worker");
    if !binary.exists() {
        return Err(format!(
            "cargo reported success but produced no worker binary at {}\n\ncargo output:\n{output}",
            binary.display()
        )
        .into());
    }

    Ok(())
}
