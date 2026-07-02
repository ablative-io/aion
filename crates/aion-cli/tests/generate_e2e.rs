//! End-to-end `aion generate` proof on `examples/order-saga`: the byte-identical
//! round-trip (C6), the `--check` drift gate in both directions (C4), and the
//! no-invented-defaults guarantee (C5). Every step drives the real `aion`
//! binary against a throwaway copy of the example, so the Gleam toolchain runs
//! exactly as it would for an author.
//!
//! Like the example-archive gates, this fails loudly when the Gleam toolchain
//! is missing rather than skipping — a green run must mean the round-trip was
//! actually proven.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestError = Box<dyn std::error::Error>;

/// The files `aion generate` produces for order-saga, relative to the
/// project root. The types module `src/aion_order_saga_io.gleam` is NOT here:
/// it is the authored source of truth (types-first, ADR-014) and generation
/// never writes it.
const GENERATED: &[&str] = &[
    "src/aion_order_saga_codecs.gleam",
    "src/aion_order_saga_activity_wrappers.gleam",
    "worker/worker.py",
    "test/aion_order_saga_wire_compat_test.gleam",
    "schemas/cancel_shipment_input.json",
    "schemas/compensation_output.json",
    "schemas/inventory_reservation.json",
    "schemas/order_input.json",
    "schemas/payment_receipt.json",
    "schemas/refund_payment_input.json",
    "schemas/release_inventory_input.json",
    "schemas/shipment.json",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Copies `examples/order-saga` into a fresh temp directory, rewriting the
/// `aion_flow` path dependency to an absolute path so it resolves from the new
/// location. Build artifacts and stale archives are not copied.
fn stage_order_saga() -> Result<(tempfile::TempDir, PathBuf), TestError> {
    let root = repo_root().canonicalize()?;
    let source = root.join("examples/order-saga");
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("order-saga");
    copy_tree(&source, &project)?;

    // The relative `aion_flow` path resolves from the example's real location;
    // rewrite it to an absolute path in both the manifest (`gleam.toml`) and the
    // lockfile (`manifest.toml`, which Gleam reads first) so it resolves from
    // the temp copy.
    let aion_flow = root.join("gleam/aion_flow");
    let absolute = format!("\"{}\"", aion_flow.display());
    for descriptor in ["gleam.toml", "manifest.toml"] {
        let path = project.join(descriptor);
        let contents = fs::read_to_string(&path)?.replace("\"../../gleam/aion_flow\"", &absolute);
        fs::write(&path, contents)?;
    }
    Ok((temp, project))
}

/// Recursively copies `from` to `to`, skipping the Gleam `build` directory and
/// any prebuilt `.aion` archive.
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

/// Runs `aion generate <project> [--check]`, returning the exit status and
/// combined output. Fails loudly when the Gleam toolchain is unavailable.
fn run_generate(project: &Path, check: bool) -> Result<(bool, String), TestError> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aion"));
    command.arg("generate").arg(project);
    if check {
        command.arg("--check");
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to spawn the `aion` binary for `generate`: {error}"))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if combined.contains("failed to run `gleam run") || combined.contains("Command not found") {
        return Err(format!(
            "`aion generate` could not drive the Gleam toolchain (is `gleam` on PATH?). \
             This gate fails loudly by design rather than skipping.\n{combined}"
        )
        .into());
    }
    Ok((output.status.success(), combined))
}

#[test]
fn generate_round_trips_check_gates_and_invents_no_defaults() -> Result<(), TestError> {
    let (_temp, project) = stage_order_saga()?;

    // The authored types module must never be written by generation.
    let types_module = project.join("src/aion_order_saga_io.gleam");
    let authored_before = fs::read(&types_module)?;

    // First generation must succeed and leave a clean tree.
    let (ok, output) = run_generate(&project, false)?;
    assert!(ok, "first `aion generate` failed:\n{output}");
    assert_eq!(
        fs::read(&types_module)?,
        authored_before,
        "generation must never touch the authored types module"
    );

    // Round-trip (C6): snapshot, delete every generated file, regenerate, and
    // require byte-identical output — this also exercises the extraction
    // isolation, since the workflow module imports the now-deleted wrappers.
    let mut snapshot = Vec::new();
    for relative in GENERATED {
        let path = project.join(relative);
        let contents = fs::read(&path)
            .map_err(|error| format!("expected {relative} to be generated: {error}"))?;
        snapshot.push((path, contents));
    }
    for (path, _) in &snapshot {
        fs::remove_file(path)?;
    }
    let (ok, output) = run_generate(&project, false)?;
    assert!(ok, "regeneration after delete failed:\n{output}");
    for (path, original) in &snapshot {
        let regenerated = fs::read(path)?;
        assert!(
            &regenerated == original,
            "round-trip drift in {}: regeneration is not byte-identical",
            path.display()
        );
    }

    // Drift gate, clean direction (C4): a clean tree passes `--check`.
    let (ok, output) = run_generate(&project, true)?;
    assert!(ok, "`--check` failed on a clean tree:\n{output}");

    // Drift gate, dirty direction (C4): a hand-edit to generated output fails.
    let wrappers = project.join("src/aion_order_saga_activity_wrappers.gleam");
    let mut tampered = fs::read_to_string(&wrappers)?;
    tampered.push_str("\n//// hand edit\n");
    fs::write(&wrappers, &tampered)?;
    let (ok, output) = run_generate(&project, true)?;
    assert!(
        !ok,
        "`--check` passed despite a hand-edited generated file:\n{output}"
    );
    assert!(
        output.contains("aion_order_saga_activity_wrappers.gleam"),
        "the --check failure must name the drifted file:\n{output}"
    );
    // Restore so the no-defaults assertion reads true generator output.
    run_generate(&project, false)?;

    // No invented defaults (C5): the generated activity wrappers carry no retry,
    // timeout, or backoff policy — only the policies an author declared, of
    // which order-saga declares none.
    let wrappers = fs::read_to_string(&wrappers)?;
    for forbidden in ["retry", "timeout", "backoff", "heartbeat", "RetryPolicy"] {
        assert!(
            !wrappers.contains(forbidden),
            "generated wrappers must not invent activity policy, found `{forbidden}`"
        );
    }

    Ok(())
}

/// Drives the real `aion generate --check` against a project whose
/// `workflow.toml` `activities` list has been desynced from the declarations,
/// and requires it to fail loudly naming workflow.toml. This is C4's drift gate
/// for the workflow.toml artifact, otherwise only proven by the library unit
/// tests; here it runs end-to-end through the built binary and the toolchain.
#[test]
fn generate_check_gates_workflow_toml_drift() -> Result<(), TestError> {
    let (_temp, project) = stage_order_saga()?;

    // A first generation establishes the schema-derived modules and a clean,
    // in-sync workflow.toml, so the only drift the leg introduces is the one it
    // injects below.
    let (ok, output) = run_generate(&project, false)?;
    assert!(ok, "first `aion generate` failed:\n{output}");

    // A clean tree must pass `--check` before we desync it, so the failure below
    // is unambiguously caused by the workflow.toml edit and nothing else.
    let (ok, output) = run_generate(&project, true)?;
    assert!(ok, "`--check` failed on a clean tree:\n{output}");

    // Desync workflow.toml: append a bogus activity name that the declarations
    // do not produce, making the list stale.
    let toml_path = project.join("workflow.toml");
    let mut toml = fs::read_to_string(&toml_path)?;
    toml = toml.replace(
        "    \"cancel_shipment\",\n",
        "    \"cancel_shipment\",\n    \"not_a_real_activity\",\n",
    );
    assert!(
        toml.contains("not_a_real_activity"),
        "the desync edit must alter the activities list"
    );
    fs::write(&toml_path, &toml)?;

    // `--check` must now exit non-zero and name the drifted artifact.
    let (ok, output) = run_generate(&project, true)?;
    assert!(
        !ok,
        "`--check` passed despite a desynced workflow.toml activities list:\n{output}"
    );
    assert!(
        output.contains("workflow.toml") && output.contains("out of date"),
        "the --check failure must reference workflow.toml and say it is out of date:\n{output}"
    );

    Ok(())
}
