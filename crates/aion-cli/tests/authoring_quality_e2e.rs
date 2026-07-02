//! End-to-end proof of the WA-007 authoring-quality scaffolds against the real
//! `examples/order-saga` and the real Gleam toolchain:
//!
//! - `aion generate` emits a `test/<entry>_scaffold_test.gleam` that compiles
//!   under `gleam build` (C29 — the skeleton targets `aion/testing` and is a
//!   valid Gleam module the author fills in).
//! - `aion check --deterministic` passes the clean order-saga workflow and fails
//!   non-zero on an injected wall-clock call (C28 — the gate proven both ways
//!   through the built binary).
//! - `aion input <workflow_type>` emits a skeleton that round-trips through the
//!   workflow's real input codec without a decode error (C30).
//!
//! The Gleam-driven legs are runtime-gated: if `gleam` is not on PATH they print
//! a skip line and pass, so the suite is green on a machine without the
//! toolchain rather than silently asserting nothing. The pure-CLI determinism
//! leg needs no toolchain and always runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestError = Box<dyn std::error::Error>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether the Gleam toolchain is available; legs that drive it skip (with a
/// printed line) when it is not, rather than failing on a toolchain-less host.
fn gleam_available() -> bool {
    Command::new("gleam")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Copies `examples/order-saga` into a fresh temp dir, rewriting the `aion_flow`
/// path dependency to an absolute path so it resolves from the new location.
fn stage_order_saga() -> Result<(tempfile::TempDir, PathBuf), TestError> {
    let root = repo_root().canonicalize()?;
    let source = root.join("examples/order-saga");
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("order-saga");
    copy_tree(&source, &project)?;

    let aion_flow = root.join("gleam/aion_flow");
    let absolute = format!("\"{}\"", aion_flow.display());
    for descriptor in ["gleam.toml", "manifest.toml"] {
        let path = project.join(descriptor);
        let contents = fs::read_to_string(&path)?.replace("\"../../gleam/aion_flow\"", &absolute);
        fs::write(&path, contents)?;
    }
    Ok((temp, project))
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), TestError> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "build" {
            continue;
        }
        let source = entry.path();
        let target = to.join(&name);
        if source.is_dir() {
            copy_tree(&source, &target)?;
        } else if source.extension().is_none_or(|ext| ext != "aion") {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Runs `aion <args...>` in (or against) the project, returning success and the
/// combined stdout+stderr.
fn run_aion(args: &[&str]) -> Result<(bool, String), TestError> {
    let output = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to spawn the `aion` binary: {error}"))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), combined))
}

#[test]
fn generated_test_scaffold_compiles_under_gleam_build() -> Result<(), TestError> {
    if !gleam_available() {
        println!("skipping: `gleam` is not on PATH; the test-scaffold compile leg is gated");
        return Ok(());
    }
    let (_temp, project) = stage_order_saga()?;
    let project_str = project.to_str().ok_or("project path is not valid UTF-8")?;

    // The example commits a filled-in scaffold (the scaffold is write-once and
    // author-owned after generation); delete it so this gate proves the
    // PRISTINE scaffold is regenerated and compiles.
    let scaffold = project.join("test/order_saga_scaffold_test.gleam");
    fs::remove_file(&scaffold)?;

    let (ok, output) = run_aion(&["generate", project_str])?;
    assert!(ok, "`aion generate` failed:\n{output}");

    // The scaffold was emitted for the single workflow.
    assert!(
        scaffold.is_file(),
        "the test scaffold was not generated at {}",
        scaffold.display()
    );
    let body = fs::read_to_string(&scaffold)?;
    // It targets the existing aion/testing harness — mocks, a clock advance, and
    // the replay-determinism assertion — and introduces no new test framework.
    assert!(body.contains("import aion/testing"));
    assert!(body.contains("testing.mock_activity("));
    assert!(body.contains("testing.assert_replay(env, fn()"));
    assert!(body.contains("workflow_under_test.execute("));

    // The whole project (including the scaffold) compiles under `gleam build`.
    let build = Command::new("gleam")
        .arg("build")
        .current_dir(&project)
        .output()
        .map_err(|error| format!("failed to run `gleam build`: {error}"))?;
    let build_output = format!(
        "{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        build.status.success(),
        "the generated scaffold did not compile under `gleam build`:\n{build_output}"
    );
    Ok(())
}

#[test]
fn determinism_gate_passes_clean_and_fails_on_injected_wall_clock() -> Result<(), TestError> {
    // This leg drives the static analysis only — no toolchain needed — so it
    // always runs.
    let (_temp, project) = stage_order_saga()?;
    let project_str = project.to_str().ok_or("project path is not valid UTF-8")?;

    // Negative fixture: the unmodified order-saga workflow reaches no wall-clock
    // or entropy call, so the gate passes (exit zero).
    let (ok, output) = run_aion(&["check", project_str, "--deterministic"])?;
    assert!(
        ok,
        "the clean order-saga workflow must pass the determinism gate:\n{output}"
    );

    // Positive fixture: inject a direct wall-clock read into the workflow's entry
    // module; the gate must now fail non-zero and name the offending call.
    let workflow_file = project.join("src/order_saga.gleam");
    let source = fs::read_to_string(&workflow_file)?;
    let tainted = source.replace(
        "pub fn execute(input: io.OrderInput) -> Result(io.Shipment, SagaFailed) {",
        "pub fn execute(input: io.OrderInput) -> Result(io.Shipment, SagaFailed) {\n  \
         let _ = erlang.system_time(1000)",
    );
    assert!(
        tainted != source,
        "the wall-clock injection must alter the workflow source"
    );
    fs::write(&workflow_file, &tainted)?;

    let (ok, output) = run_aion(&["check", project_str, "--deterministic"])?;
    assert!(
        !ok,
        "the tainted workflow must fail the determinism gate non-zero:\n{output}"
    );
    assert!(
        output.contains("erlang.system_time"),
        "the gate failure must name the offending wall-clock call:\n{output}"
    );
    Ok(())
}

#[test]
fn input_skeleton_round_trips_through_the_workflow_input_codec() -> Result<(), TestError> {
    let (_temp, project) = stage_order_saga()?;
    let project_str = project.to_str().ok_or("project path is not valid UTF-8")?;

    // The skeleton is derived from the workflow's input type (its `input_schema`).
    let (ok, output) = run_aion(&["input", "order_saga", project_str])?;
    assert!(ok, "`aion input` failed:\n{output}");
    let skeleton: serde_json::Value = serde_json::from_str(skeleton_json(&output))
        .map_err(|error| format!("`aion input` did not emit valid JSON: {error}\n{output}"))?;
    assert!(
        skeleton.is_object(),
        "the input skeleton must be a JSON object: {skeleton}"
    );

    if !gleam_available() {
        println!(
            "skipping codec round-trip: `gleam` is not on PATH; the structural skeleton check ran"
        );
        return Ok(());
    }

    // Round-trip the skeleton through the workflow's REAL generated input codec
    // under `gleam test`: a one-off Gleam test module decodes the emitted
    // skeleton and asserts it is `Ok`. First `aion generate` so the codecs exist.
    let (ok, output) = run_aion(&["generate", project_str])?;
    assert!(
        ok,
        "`aion generate` failed before the codec round-trip:\n{output}"
    );

    let skeleton_string = serde_json::to_string(&skeleton)?;
    let escaped = skeleton_string.replace('\\', "\\\\").replace('"', "\\\"");
    let test_module = format!(
        "import aion_order_saga_codecs as codecs\n\
         import gleeunit\n\
         import gleeunit/should\n\n\
         pub fn main() {{\n  gleeunit.main()\n}}\n\n\
         pub fn input_skeleton_round_trips_test() {{\n  \
         let skeleton = \"{escaped}\"\n  \
         codecs.order_input_codec().decode(skeleton)\n  \
         |> should.be_ok\n}}\n"
    );
    let test_path = project.join("test/order_saga_input_skeleton_test.gleam");
    fs::write(&test_path, test_module)?;
    // The generated scaffold is a deliberately-unfilled skeleton (its `todo`
    // holes panic at runtime); remove it so this codec round-trip leg's
    // `gleam test` exercises only the round-trip assertion, not the scaffold.
    let scaffold = project.join("test/order_saga_scaffold_test.gleam");
    if scaffold.exists() {
        fs::remove_file(&scaffold)?;
    }

    let test_run = Command::new("gleam")
        .arg("test")
        .current_dir(&project)
        .output()
        .map_err(|error| format!("failed to run `gleam test`: {error}"))?;
    let test_output = format!(
        "{}{}",
        String::from_utf8_lossy(&test_run.stdout),
        String::from_utf8_lossy(&test_run.stderr)
    );
    assert!(
        test_run.status.success(),
        "the input skeleton did not round-trip through the input codec:\n{test_output}"
    );
    Ok(())
}

/// Extracts the JSON document from `aion input` output, which prints the
/// skeleton as compact JSON on stdout (the only line that parses as JSON).
fn skeleton_json(output: &str) -> &str {
    output
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('{'))
        .unwrap_or(output.trim())
}
