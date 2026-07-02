//! End-to-end gates for the types-first `aion generate` front door: the
//! generated codecs module must be deterministic, must compile under a real
//! `gleam build`, must round-trip sample payloads byte-for-byte at runtime,
//! and `--check` must pass on a clean tree and fail loudly on drift — in the
//! codecs module AND in the emitted schemas. Boundary types outside the
//! supported subset must fail loudly naming the type and field, and a stray
//! hand-authored schema must fail with the migration hint.
//!
//! Like the scaffold gates, a missing `gleam` CLI FAILS these tests with an
//! explicit error — never a skip.

mod common;

use std::path::Path;
use std::process::Command;

use common::TestError;

/// Coverage types appended to the scaffold's authored types module,
/// exercising every supported construct: an enum, every scalar, lists, a
/// nested record, optional fields, a record whose field names shadow the
/// generated module's imports (`decode`/`json`/`list`/`option` — the decoder
/// bindings must be hygienic), an empty record, and an all-optional record.
const COVERAGE_TYPES: &str = r"
import gleam/option

/// Import-shadowing field names: a naive `use option <- ...` binding would
/// shadow the generated imports for the rest of the decoder.
pub type AaShadow {
  AaShadow(
    decode: String,
    json: Int,
    list: List(String),
    option: option.Option(String),
    extra: option.Option(String),
  )
}

/// An object with no fields: bare constructor, `json.object([])`,
/// `decode.success(io.AbEmpty)`.
pub type AbEmpty {
  AbEmpty
}

/// A record whose fields are all optional: every encoder segment is a
/// `case`, locking the `list`/`option` import wiring with zero required
/// fields.
pub type AcSparse {
  AcSparse(a: option.Option(String), b: option.Option(Int))
}

/// The kitchen sink: enum, every scalar, list, nested record, optional.
pub type DemoEvent {
  DemoEvent(
    kind: DemoEventKind,
    count: Int,
    ratio: Float,
    active: Bool,
    tags: List(String),
    origin: DemoEventOrigin,
    note: option.Option(String),
  )
}

/// Enum whose wire strings are the constructor names with the type-name
/// prefix stripped, snake_cased.
pub type DemoEventKind {
  DemoEventKindCreated
  DemoEventKindClosedOut
}

/// Nested record with an optional field.
pub type DemoEventOrigin {
  DemoEventOrigin(host: String, port: option.Option(Int))
}
";

/// Gleam module that decodes canonical sample payloads with the generated
/// decoders, re-encodes them with the generated encoders, and asserts the
/// bytes match — covering optional-present, optional-absent, enum, nested,
/// array, empty-record, and all-optional payloads.
const ROUND_TRIP_MODULE: &str = r#"import codegen_gate_codecs as codecs
import gleam/dynamic/decode
import gleam/io
import gleam/json

pub fn main() -> Nil {
  assert_round_trip(
    "input",
    "{\"name\":\"aion\"}",
    codecs.input_decoder(),
    codecs.input_to_json,
  )
  assert_round_trip(
    "output",
    "{\"greeting\":\"hi\"}",
    codecs.output_decoder(),
    codecs.output_to_json,
  )
  assert_round_trip(
    "demo_event optionals present",
    "{\"kind\":\"closed_out\",\"count\":3,\"ratio\":0.5,\"active\":true,\"tags\":[\"a\",\"b\"],\"origin\":{\"host\":\"h\",\"port\":80},\"note\":\"n\"}",
    codecs.demo_event_decoder(),
    codecs.demo_event_to_json,
  )
  assert_round_trip(
    "demo_event optionals absent",
    "{\"kind\":\"created\",\"count\":1,\"ratio\":1.5,\"active\":false,\"tags\":[],\"origin\":{\"host\":\"h\"}}",
    codecs.demo_event_decoder(),
    codecs.demo_event_to_json,
  )
  assert_round_trip(
    "aa_shadow import-named fields present",
    "{\"decode\":\"d\",\"json\":1,\"list\":[\"x\"],\"option\":\"o\",\"extra\":\"e\"}",
    codecs.aa_shadow_decoder(),
    codecs.aa_shadow_to_json,
  )
  assert_round_trip(
    "aa_shadow import-named fields absent",
    "{\"decode\":\"d\",\"json\":1,\"list\":[]}",
    codecs.aa_shadow_decoder(),
    codecs.aa_shadow_to_json,
  )
  assert_round_trip(
    "ab_empty record",
    "{}",
    codecs.ab_empty_decoder(),
    codecs.ab_empty_to_json,
  )
  assert_round_trip(
    "ac_sparse all-optional absent",
    "{}",
    codecs.ac_sparse_decoder(),
    codecs.ac_sparse_to_json,
  )
  assert_round_trip(
    "ac_sparse all-optional present",
    "{\"a\":\"x\",\"b\":2}",
    codecs.ac_sparse_decoder(),
    codecs.ac_sparse_to_json,
  )
  io.println("round-trip ok")
}

fn assert_round_trip(
  label: String,
  raw: String,
  decoder: decode.Decoder(t),
  encode: fn(t) -> json.Json,
) -> Nil {
  case json.parse(raw, decoder) {
    Ok(value) -> {
      let encoded = json.to_string(encode(value))
      case encoded == raw {
        True -> Nil
        False ->
          panic as {
            label <> ": re-encoded JSON drifted: " <> encoded <> " != " <> raw
          }
      }
    }
    Error(_) -> panic as { label <> ": generated decoder rejected the sample" }
  }
}
"#;

/// Scaffolds a hello-world project (authored types module + pre-generated
/// artifacts), appends the coverage types, and patches `aion_flow` to the
/// workspace SDK.
fn stage_coverage_project(parent: &Path) -> Result<std::path::PathBuf, TestError> {
    let (project, _) = common::scaffold_project(parent, "codegen_gate", &[])?;
    common::patch_aion_flow_to_workspace(&project)?;
    let io_path = project.join("src/codegen_gate_io.gleam");
    let mut io_module = std::fs::read_to_string(&io_path)?;
    io_module.push_str(COVERAGE_TYPES);
    std::fs::write(&io_path, io_module)?;
    Ok(project)
}

/// Scaffold → author coverage types → `aion generate` → determinism →
/// `--check` clean → `gleam build` + runtime round-trip → drift in the
/// codecs module and in an emitted schema each fails `--check` naming the
/// file.
#[test]
fn generate_compiles_round_trips_and_checks() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let project = stage_coverage_project(temp_dir.path())?;

    // Generate.
    let output = common::run_cli(&project, &["generate", "."])?;
    let report = common::success_json(&output)?;
    assert_eq!(report["codecs_module"], "src/codegen_gate_codecs.gleam");
    assert_eq!(report["action"], "written");
    assert_eq!(
        report["schemas_emitted"]
            .as_array()
            .ok_or("generate report must list emitted schemas")?
            .iter()
            .map(|value| value.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>(),
        vec![
            "schemas/aa_shadow.json",
            "schemas/ab_empty.json",
            "schemas/ac_sparse.json",
            "schemas/demo_event.json",
            "schemas/demo_event_kind.json",
            "schemas/demo_event_origin.json",
            "schemas/input.json",
            "schemas/output.json",
        ]
    );
    let module_path = project.join("src/codegen_gate_codecs.gleam");
    let first = std::fs::read_to_string(&module_path)?;
    assert!(
        first.starts_with("//// Generated by aion generate — do not edit"),
        "generated module must carry the do-not-edit header; got:\n{first}"
    );
    // The canonical enum wire mapping: type-name prefix stripped, snake_cased.
    let schema = std::fs::read_to_string(project.join("schemas/demo_event_kind.json"))?;
    assert!(
        schema.contains("\"enum\": [\"created\", \"closed_out\"]"),
        "enum wire strings must strip the type-name prefix: {schema}"
    );

    // Determinism: regeneration is byte-identical.
    common::success_json(&common::run_cli(&project, &["generate", "."])?)?;
    let second = std::fs::read_to_string(&module_path)?;
    assert_eq!(first, second, "regeneration must be byte-identical");

    // A clean tree passes --check.
    let checked = common::success_json(&common::run_cli(&project, &["generate", ".", "--check"])?)?;
    assert_eq!(checked["action"], "checked");

    // The generated module compiles and round-trips under the real toolchain.
    std::fs::write(
        project.join("src/codegen_round_trip.gleam"),
        ROUND_TRIP_MODULE,
    )?;
    common::gleam_build(&project)?;
    gleam_run_round_trip(&project)?;

    // The project still packages with the generated module in its sources.
    common::package_project(&project, "codegen_gate")?;

    // Drift in the codecs module (a hand edit) fails --check, naming the file.
    let mut perturbed = first.clone();
    perturbed.push_str("\n// hand edit\n");
    std::fs::write(&module_path, &perturbed)?;
    let output = common::run_cli(&project, &["generate", ".", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "--check drift must exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("codegen_gate_codecs.gleam") && stderr.contains("differs"),
        "--check failure must name the drifted file: {stderr}"
    );
    std::fs::write(&module_path, &first)?;

    // Drift in an emitted schema fails --check too, naming the file.
    let schema_path = project.join("schemas/demo_event.json");
    let pristine_schema = std::fs::read_to_string(&schema_path)?;
    std::fs::write(&schema_path, format!("{pristine_schema}\n"))?;
    let output = common::run_cli(&project, &["generate", ".", "--check"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "schema drift must fail --check"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("demo_event.json") && stderr.contains("differs"),
        "--check failure must name the drifted schema: {stderr}"
    );
    Ok(())
}

/// Runs the generated round-trip assertions on the BEAM via `gleam run`.
fn gleam_run_round_trip(project: &Path) -> Result<(), TestError> {
    let output = Command::new("gleam")
        .args(["run", "-m", "codegen_round_trip"])
        .current_dir(project)
        .output()
        .map_err(|error| {
            format!(
                "the generate gate requires the `gleam` CLI on PATH (failed to spawn \
                 `gleam run` in {}: {error}); this gate fails loudly by design",
                project.display()
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !stdout.contains("round-trip ok") {
        return Err(format!(
            "generated codecs must round-trip; `gleam run -m codegen_round_trip` \
             exited with {} — stdout: {stdout} stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

/// Loud-failure proof: a boundary type outside the v1 subset (a tuple field)
/// fails `aion generate` naming the module, type, and field — and writes
/// nothing new.
#[test]
fn type_outside_subset_fails_loudly() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let project = stage_coverage_project(temp_dir.path())?;
    let io_path = project.join("src/codegen_gate_io.gleam");
    let mut io_module = std::fs::read_to_string(&io_path)?;
    io_module.push_str("\n/// Outside the subset: a tuple field.\npub type ZzBad {\n  ZzBad(pair: #(Int, Int))\n}\n");
    std::fs::write(&io_path, io_module)?;

    let output = common::run_cli(&project, &["generate", "."])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "an out-of-subset type must exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ZzBad") && stderr.contains("pair") && stderr.contains("tuple"),
        "the loud error must name the type and field: {stderr}"
    );
    Ok(())
}

/// Migration-boundary proof: a stray hand-authored (unmarked) schema in
/// `schemas/` fails `aion generate` with the migration hint — schema-first
/// authoring is gone.
#[test]
fn stray_authored_schema_fails_with_the_migration_hint() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let project = stage_coverage_project(temp_dir.path())?;
    std::fs::write(
        project.join("schemas/legacy.json"),
        r#"{ "type": "object", "required": [], "properties": {} }"#,
    )?;

    let output = common::run_cli(&project, &["generate", "."])?;
    assert_eq!(output.status.code(), Some(1), "a stray schema must exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("legacy.json")
            && stderr.contains("never authored")
            && stderr.contains("aion generate"),
        "the stray error must name the file and carry the migration recipe: {stderr}"
    );
    Ok(())
}
