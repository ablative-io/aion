//! Executes a generated wire-compat golden against a real `gleam test`, proving
//! the golden's type-derived literal is byte-exact what `gleam_json` produces
//! on the Erlang target for the risky value shapes — a Float (`0.0`, never `0`),
//! an optional field omitted from the wire, an enum rendered as its first
//! variant's wire string, and a nested record — none of which the all-`String`/
//! `Int` `examples/order-saga` golden ever exercises (checklist C3).
//!
//! Types-first: the risky shapes are AUTHORED as Gleam types appended to the
//! example's types module, the real compiler exports the package interface,
//! and the whole pipeline (interface → model → codecs module → emitted
//! schemas → wrappers/golden) runs through the `aion_package` library API —
//! never the `aion` binary — before `gleam test` executes the golden. The
//! generator derives both the canonical Gleam sample and the expected JSON
//! literal from the same model the codecs come from, so the only thing that
//! can make this fail is a genuine disagreement between the derived literal
//! and real `gleam_json`. A second leg deletes every generated file and
//! regenerates, asserting the round-trip is byte-identical.
//!
//! Per project policy this never uses `#[ignore]`: when the `gleam` toolchain is
//! absent it logs a skip line and returns `Ok(())`; when `gleam` is present it
//! runs `gleam test` for real and fails the Rust test if that exits non-zero.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_package::{
    ActivityDeclaration, CodegenMode, Tier, boundary_types_from_interface, emit_schemas,
    generate_activities, generate_codecs,
};

type TestError = Box<dyn std::error::Error>;

/// The authored Gleam types that exercise every risky wire shape in one value
/// type, appended to the example's types module and used as the input of the
/// `golden_probe` activity.
///
/// - `ratio: Float` (required) → a `Float`, whose zero value is `0.0` (the
///   shape order-saga never has — its only numbers are `Int`).
/// - `line: GoldenInputLine` (required) → a nested record, whose wire literal
///   is a nested `{...}` object.
/// - `kind: GoldenInputKind` (required) → an enum, whose sample is the first
///   variant and whose literal is that variant's wire string (the type-name
///   prefix stripped: `GoldenInputKindStandard` → `"standard"`).
/// - `note: option.Option(String)` → an optional field whose sample is
///   `option.None` and is omitted from the wire literal entirely.
const GOLDEN_INPUT_TYPES: &str = r"
import gleam/option

/// Wire-shape probe input for the golden gate.
pub type GoldenInput {
  GoldenInput(
    ratio: Float,
    line: GoldenInputLine,
    kind: GoldenInputKind,
    note: option.Option(String),
  )
}

/// Nested record on the probe input.
pub type GoldenInputLine {
  GoldenInputLine(sku: String)
}

/// Enum on the probe input.
pub type GoldenInputKind {
  GoldenInputKindStandard
  GoldenInputKindRush
}
";

/// The hand-written stub body the generated `golden_probe` wrapper references
/// via `activities.golden_probe`. It never executes during `gleam test` (the
/// wire-compat golden only encodes a sample value), but it must type-check, so
/// it consumes `io.GoldenInput` and returns the existing `io.Shipment`.
const GOLDEN_PROBE_BODY: &str = "
pub fn golden_probe(
  _input: io.GoldenInput,
) -> Result(io.Shipment, error.ActivityError) {
  Ok(io.Shipment(order_id: \"\", shipment_id: \"\"))
}
";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether the `gleam` toolchain can be run. Per project policy the test is
/// gated at runtime rather than with `#[ignore]`: absence logs and skips,
/// presence runs for real.
fn gleam_available() -> bool {
    Command::new("gleam")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Copies `examples/order-saga` into a fresh temp directory, rewriting the
/// `aion_flow` path dependency to an absolute path in both `gleam.toml` and the
/// lockfile `manifest.toml` so it resolves from the new location. Mirrors
/// `crates/aion-cli/tests/generate_e2e.rs`. Build artifacts and stale archives
/// are not copied.
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

/// The order-saga activities the staged workflow module still references; the
/// generated wrappers must keep covering them, so they are re-declared verbatim
/// alongside the new `golden_probe` probe. Each tuple is
/// `(name, input_type, output_type)` and every one is `RemotePython` so it
/// contributes to the wire-compat golden.
const ORDER_SAGA_ACTIVITIES: &[(&str, &str, &str)] = &[
    ("reserve_inventory", "OrderInput", "InventoryReservation"),
    ("charge_payment", "OrderInput", "PaymentReceipt"),
    ("ship_order", "OrderInput", "Shipment"),
    (
        "release_inventory",
        "ReleaseInventoryInput",
        "CompensationOutput",
    ),
    ("refund_payment", "RefundPaymentInput", "CompensationOutput"),
    (
        "cancel_shipment",
        "CancelShipmentInput",
        "CompensationOutput",
    ),
];

/// Builds the declaration list: the existing order-saga activities (so their
/// wrappers, which the workflow module calls, keep existing) plus `golden_probe`
/// carrying the risky `GoldenInput` value type.
fn declarations() -> Vec<ActivityDeclaration> {
    let mut declarations: Vec<ActivityDeclaration> = ORDER_SAGA_ACTIVITIES
        .iter()
        .map(|(name, input, output)| ActivityDeclaration {
            name: (*name).to_owned(),
            tier: Tier::RemotePython,
            input_type: (*input).to_owned(),
            output_type: (*output).to_owned(),
        })
        .collect();
    declarations.push(ActivityDeclaration {
        name: "golden_probe".to_owned(),
        tier: Tier::RemotePython,
        input_type: "GoldenInput".to_owned(),
        output_type: "Shipment".to_owned(),
    });
    declarations
}

/// Runs `gleam test` in `project`, returning whether it succeeded and the
/// combined stdout+stderr.
fn run_gleam_test(project: &Path) -> Result<(bool, String), TestError> {
    let output = Command::new("gleam")
        .arg("test")
        .current_dir(project)
        .output()
        .map_err(|error| format!("failed to spawn `gleam test`: {error}"))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), combined))
}

/// Runs `gleam export package-interface` in `project` and returns the JSON.
/// The staged example compiles as committed (the appended probe types are
/// additive), so no rename-aside dance is needed here.
fn export_interface(project: &Path) -> Result<Vec<u8>, TestError> {
    let out = project.join("build/test-interface.json");
    let output = Command::new("gleam")
        .args(["export", "package-interface", "--out"])
        .arg(&out)
        .current_dir(project)
        .output()
        .map_err(|error| format!("failed to spawn `gleam export package-interface`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`gleam export package-interface` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(fs::read(&out)?)
}

#[test]
fn generated_golden_passes_real_gleam_test_for_float_optional_enum_and_nested()
-> Result<(), TestError> {
    if !gleam_available() {
        eprintln!(
            "skipping golden_gleam_exec: gleam toolchain not on PATH \
             (run `gleam --version` to confirm); this gate runs for real when gleam is present"
        );
        return Ok(());
    }

    let (_temp, project) = stage_order_saga()?;

    // Author the types that introduce the Float, optional, enum, and nested
    // record into the types module — the types-first front door.
    let io_path = project.join("src/aion_order_saga_io.gleam");
    let mut io_module = fs::read_to_string(&io_path)?;
    io_module.push_str(GOLDEN_INPUT_TYPES);
    fs::write(&io_path, io_module)?;

    // The generated `golden_probe` wrapper references `activities.golden_probe`;
    // give the hand-written activities module a type-checking stub so the
    // project compiles as a whole.
    let activities_path = project.join("src/aion_order_saga_activities.gleam");
    let mut activities = fs::read_to_string(&activities_path)?;
    activities.push_str(GOLDEN_PROBE_BODY);
    fs::write(&activities_path, activities)?;

    // Run the full types-first pipeline through the library API: interface →
    // model → codecs module + emitted schemas → activity plumbing (including
    // the wire-compat golden the rest of this test executes).
    let package_name = "aion_order_saga";
    let interface = export_interface(&project)?;
    let types = boundary_types_from_interface(&interface, package_name)?;
    generate_codecs(&project, &types, CodegenMode::Write)?;
    emit_schemas(&project, package_name, &types, CodegenMode::Write)?;
    generate_activities(&project, &declarations(), &types, CodegenMode::Write)?;

    // Cheap structural guard: assert the generated golden actually exercises
    // each risky shape, so a future generator change that stops emitting one is
    // caught here even before `gleam test` runs.
    let golden_path = project.join("test/aion_order_saga_wire_compat_test.gleam");
    let golden = fs::read_to_string(&golden_path)?;
    let probe_test = golden
        .split("pub fn golden_input_wire_test()")
        .nth(1)
        .ok_or("generated golden has no golden_input_wire_test (GoldenInput not pinned)")?;
    // Stop at the next test function so the asserts below describe only the
    // GoldenInput case.
    let probe_test = probe_test.split("\npub fn ").next().unwrap_or(probe_test);

    let mut missing = Vec::new();
    // Float zero value, never the integer `0`.
    if !probe_test.contains("ratio: 0.0") || !probe_test.contains("\\\"ratio\\\":0.0") {
        missing.push("Float `0.0` (sample `ratio: 0.0` and literal `\\\"ratio\\\":0.0`)");
    }
    // Nested record: a `{...}` object literal for the `line` field.
    if !probe_test.contains("line: io.GoldenInputLine(sku: \"\")")
        || !probe_test.contains("\\\"line\\\":{\\\"sku\\\":\\\"\\\"}")
    {
        missing.push("nested record `line` (sample `io.GoldenInputLine(...)` and `{...}` literal)");
    }
    // Enum: first variant constructor and its wire string.
    if !probe_test.contains("kind: io.GoldenInputKindStandard")
        || !probe_test.contains("\\\"kind\\\":\\\"standard\\\"")
    {
        missing.push("enum `kind` (first variant `GoldenInputKindStandard` / wire `standard`)");
    }
    // Optional field omitted: `note` is `option.None` in the sample and absent
    // from the literal.
    if !probe_test.contains("note: option.None") {
        missing.push("optional `note` as `option.None` in the sample");
    }
    if probe_test.contains("\\\"note\\\"") {
        missing.push("optional `note` MUST be omitted from the wire literal but appears in it");
    }
    if !missing.is_empty() {
        return Err(format!(
            "generated golden_input_wire_test does not exercise the risky shapes it must \
             ({}). The generated test was:\n{probe_test}",
            missing.join("; ")
        )
        .into());
    }

    // Round-trip: delete every generated file and regenerate — byte-identical.
    let generated = [
        "src/aion_order_saga_codecs.gleam",
        "src/aion_order_saga_activity_wrappers.gleam",
        "worker/worker.py",
        "test/aion_order_saga_wire_compat_test.gleam",
        "schemas/golden_input.json",
        "schemas/order_input.json",
    ];
    let mut snapshot = Vec::new();
    for relative in generated {
        let path = project.join(relative);
        snapshot.push((relative, fs::read(&path)?));
        fs::remove_file(&path)?;
    }
    generate_codecs(&project, &types, CodegenMode::Write)?;
    emit_schemas(&project, package_name, &types, CodegenMode::Write)?;
    generate_activities(&project, &declarations(), &types, CodegenMode::Write)?;
    for (relative, original) in &snapshot {
        let regenerated = fs::read(project.join(relative))?;
        if &regenerated != original {
            return Err(
                format!("round-trip drift in {relative}: regeneration not byte-identical").into(),
            );
        }
    }

    // The proof: run the generated golden through real `gleam_json` on the BEAM.
    // A non-zero exit means the type-derived literal disagrees with what
    // `gleam_json` actually produces — exactly the latent wire-compat bug this
    // gate exists to surface — so it fails the Rust test with gleam's output.
    let (ok, combined) = run_gleam_test(&project)?;
    if !ok {
        return Err(format!(
            "`gleam test` failed on the generated wire-compat golden — the type-derived \
             literal does not match real `gleam_json` output. This is a wire-compat BLOCKER, \
             not a flaky test.\n\n{combined}"
        )
        .into());
    }

    Ok(())
}
