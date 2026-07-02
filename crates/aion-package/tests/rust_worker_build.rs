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
//! in a temp dir, maps a captured package-interface document through the real
//! types-first front end ([`boundary_types_from_interface`]), drives the
//! library generators ([`generate_codecs`], [`generate_activities`]) to emit
//! the do-not-edit `worker/src/main.rs`, wraps it in a compilable crate with a
//! `path` dependency on the in-tree `aion-worker`, supplies a hand-written
//! `handlers.rs` whose stub signatures match what `register_activity` requires,
//! and runs a nested `cargo build`. A non-zero build is a real generator/SDK
//! mismatch and fails this test, surfacing `cargo`'s own stderr.
//!
//! The boundary types deliberately exercise the emitter's interesting paths:
//! an optional field (`Option<T>` + `#[serde(default, skip_serializing_if = …)]`),
//! a Rust-keyword field name (`match` → `r#match`), an enum, a nested
//! record, and a list (`Vec<T>`). `match` is a Rust keyword that is a valid
//! Gleam label, so it is the honest way to drive the `r#`-escaping path
//! through the real pipeline.
//!
//! The nested build pulls the worker's full dependency tree (tokio, tonic) and
//! is slow on a cold cache — that is the point, and it is acceptable. The build
//! runs in an isolated `CARGO_TARGET_DIR` so it never pollutes the workspace
//! target directory, and every temp directory drops at the end of the test.

use std::path::{Path, PathBuf};
use std::process::Command;

use aion_package::{
    ActivityDeclaration, CodegenMode, Tier, boundary_types_from_interface, emit_schemas,
    generate_activities, generate_codecs,
};

type TestError = Box<dyn std::error::Error>;

/// `gleam.toml` naming the generated module prefix (`src/demo_*.gleam`).
const GLEAM_TOML: &str = "name = \"demo\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n";

/// The exported package interface for the fixture's types module, in the
/// exact shape `gleam export package-interface` produces. The activity input
/// type (`ChargeInput`) drives every interesting emitter path: a required
/// scalar, an optional scalar (`Option<String>`), a list (`Vec<String>`), a
/// Rust-keyword field name (`match` → `r#match`), a nested record
/// (`ChargeInputCustomer`, with its own optional `vip`), and an enum
/// (`ChargeInputKind`). The output type (`Receipt`) is a single required
/// scalar field, enough for the handler stub to construct a value with no
/// imports.
const INTERFACE_JSON: &str = r#"{
  "name": "demo",
  "version": "0.1.0",
  "modules": {
    "demo_io": {
      "type-aliases": {}, "constants": {}, "functions": {},
      "types": {
        "ChargeInput": {
          "parameters": 0,
          "constructors": [{
            "name": "ChargeInput",
            "parameters": [
              { "label": "order_id", "type": { "kind": "named", "name": "String", "module": "gleam", "package": "", "parameters": [] } },
              { "label": "note", "type": { "kind": "named", "name": "Option", "module": "gleam/option", "package": "gleam_stdlib", "parameters": [ { "kind": "named", "name": "String", "module": "gleam", "package": "", "parameters": [] } ] } },
              { "label": "tags", "type": { "kind": "named", "name": "List", "module": "gleam", "package": "", "parameters": [ { "kind": "named", "name": "String", "module": "gleam", "package": "", "parameters": [] } ] } },
              { "label": "match", "type": { "kind": "named", "name": "String", "module": "gleam", "package": "", "parameters": [] } },
              { "label": "customer", "type": { "kind": "named", "name": "ChargeInputCustomer", "module": "demo_io", "package": "demo", "parameters": [] } },
              { "label": "kind", "type": { "kind": "named", "name": "ChargeInputKind", "module": "demo_io", "package": "demo", "parameters": [] } }
            ]
          }]
        },
        "ChargeInputCustomer": {
          "parameters": 0,
          "constructors": [{
            "name": "ChargeInputCustomer",
            "parameters": [
              { "label": "customer_id", "type": { "kind": "named", "name": "String", "module": "gleam", "package": "", "parameters": [] } },
              { "label": "vip", "type": { "kind": "named", "name": "Option", "module": "gleam/option", "package": "gleam_stdlib", "parameters": [ { "kind": "named", "name": "Bool", "module": "gleam", "package": "", "parameters": [] } ] } }
            ]
          }]
        },
        "ChargeInputKind": {
          "parameters": 0,
          "constructors": [
            { "name": "ChargeInputKindStandard", "parameters": [] },
            { "name": "ChargeInputKindExpress", "parameters": [] }
          ]
        },
        "Receipt": {
          "parameters": 0,
          "constructors": [{
            "name": "Receipt",
            "parameters": [
              { "label": "payment_id", "type": { "kind": "named", "name": "String", "module": "gleam", "package": "", "parameters": [] } }
            ]
          }]
        }
      }
    }
  }
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

/// Lays down the minimal source project the generators read: a `gleam.toml`
/// and an empty `src/`.
fn stage_source_project(root: &Path) -> Result<(), TestError> {
    write_file(root, "gleam.toml", GLEAM_TOML.as_bytes())?;
    std::fs::create_dir_all(root.join("src"))?;
    Ok(())
}

/// Runs the library generators that emit `worker/src/main.rs`, in the order the
/// CLI uses: the interface-derived model first, then the codecs module +
/// emitted schemas, then the declaration-driven plumbing.
fn generate_worker(root: &Path) -> Result<(), TestError> {
    // The model comes through the real interface front end, and the codecs
    // module and schema artifacts are emitted too; running them here proves
    // the whole generation path the worker rides on, not just the Rust
    // emitter in isolation.
    let types = boundary_types_from_interface(INTERFACE_JSON.as_bytes(), "demo")?;
    generate_codecs(root, &types, CodegenMode::Write)?;
    emit_schemas(root, "demo", &types, CodegenMode::Write)?;

    let declarations = vec![ActivityDeclaration {
        name: "charge_payment".to_owned(),
        tier: Tier::RemoteRust,
        input_type: "ChargeInput".to_owned(),
        output_type: "Receipt".to_owned(),
    }];
    generate_activities(root, &declarations, &types, CodegenMode::Write)?;

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
