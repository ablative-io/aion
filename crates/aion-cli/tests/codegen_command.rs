//! End-to-end gates for `aion codegen`: the generated module must be
//! deterministic, must compile under a real `gleam build`, must round-trip
//! sample payloads byte-for-byte at runtime, and `--check` must pass on a
//! clean tree and fail loudly on drift. The generator is also proven against
//! the real stacked-dev example schemas.
//!
//! Like the scaffold gates, a missing `gleam` CLI FAILS these tests with an
//! explicit error — never a skip.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::TestError;

/// A coverage schema exercising every supported construct: string enum,
/// every scalar, arrays, a nested object, and optional fields (both a
/// top-level one and one inside the nested object).
const DEMO_EVENT_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["kind", "count", "ratio", "active", "tags", "origin"],
  "additionalProperties": false,
  "properties": {
    "kind": { "type": "string", "enum": ["created", "closed_out"] },
    "count": { "type": "integer" },
    "ratio": { "type": "number" },
    "active": { "type": "boolean" },
    "tags": { "type": "array", "items": { "type": "string" } },
    "origin": {
      "type": "object",
      "required": ["host"],
      "additionalProperties": false,
      "properties": {
        "host": { "type": "string" },
        "port": { "type": "integer" }
      }
    },
    "note": { "type": "string" }
  }
}"#;

/// A scalar-root schema, covering the payload-wrapper emission path.
const LABEL_SCHEMA: &str = r#"{ "type": "string" }"#;

/// Properties named after the module's generated imports (`decode`,
/// `option`, `json`, `list`): decoder bindings must be hygienic so these
/// never shadow the imports — `option` is deliberately an optional field
/// followed by another optional field, the exact shape that would break a
/// naive `use option <- ...` binding.
const SHADOW_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["decode", "json", "list"],
  "additionalProperties": false,
  "properties": {
    "decode": { "type": "string" },
    "json": { "type": "integer" },
    "list": { "type": "array", "items": { "type": "string" } },
    "option": { "type": "string" },
    "extra": { "type": "string" }
  }
}"#;

/// An object with no properties: bare constructor, `json.object([])`,
/// `decode.success(Name)`.
const EMPTY_SCHEMA: &str = r#"{
  "type": "object",
  "required": [],
  "additionalProperties": false,
  "properties": {}
}"#;

/// A record whose fields are all optional: every encoder segment is a
/// `case`, locking the `list`/`option` import wiring with zero required
/// fields.
const SPARSE_SCHEMA: &str = r#"{
  "type": "object",
  "required": [],
  "additionalProperties": false,
  "properties": {
    "a": { "type": "string" },
    "b": { "type": "integer" }
  }
}"#;

/// Gleam module that decodes canonical sample payloads with the generated
/// decoders, re-encodes them with the generated encoders, and asserts the
/// bytes match — covering optional-present, optional-absent, enum, nested,
/// array, and scalar-root payloads.
const ROUND_TRIP_MODULE: &str = r#"import codegen_gate_io
import gleam/dynamic/decode
import gleam/io
import gleam/json

pub fn main() -> Nil {
  assert_round_trip(
    "input",
    "{\"name\":\"aion\"}",
    codegen_gate_io.input_decoder(),
    codegen_gate_io.input_to_json,
  )
  assert_round_trip(
    "output",
    "{\"greeting\":\"hi\"}",
    codegen_gate_io.output_decoder(),
    codegen_gate_io.output_to_json,
  )
  assert_round_trip(
    "demo_event optionals present",
    "{\"kind\":\"closed_out\",\"count\":3,\"ratio\":0.5,\"active\":true,\"tags\":[\"a\",\"b\"],\"origin\":{\"host\":\"h\",\"port\":80},\"note\":\"n\"}",
    codegen_gate_io.demo_event_decoder(),
    codegen_gate_io.demo_event_to_json,
  )
  assert_round_trip(
    "demo_event optionals absent",
    "{\"kind\":\"created\",\"count\":1,\"ratio\":1.5,\"active\":false,\"tags\":[],\"origin\":{\"host\":\"h\"}}",
    codegen_gate_io.demo_event_decoder(),
    codegen_gate_io.demo_event_to_json,
  )
  assert_round_trip(
    "zz_label scalar root",
    "\"done\"",
    codegen_gate_io.zz_label_decoder(),
    codegen_gate_io.zz_label_to_json,
  )
  assert_round_trip(
    "aa_shadow import-named fields present",
    "{\"decode\":\"d\",\"json\":1,\"list\":[\"x\"],\"option\":\"o\",\"extra\":\"e\"}",
    codegen_gate_io.aa_shadow_decoder(),
    codegen_gate_io.aa_shadow_to_json,
  )
  assert_round_trip(
    "aa_shadow import-named fields absent",
    "{\"decode\":\"d\",\"json\":1,\"list\":[]}",
    codegen_gate_io.aa_shadow_decoder(),
    codegen_gate_io.aa_shadow_to_json,
  )
  assert_round_trip(
    "ab_empty record",
    "{}",
    codegen_gate_io.ab_empty_decoder(),
    codegen_gate_io.ab_empty_to_json,
  )
  assert_round_trip(
    "ac_sparse all-optional absent",
    "{}",
    codegen_gate_io.ac_sparse_decoder(),
    codegen_gate_io.ac_sparse_to_json,
  )
  assert_round_trip(
    "ac_sparse all-optional present",
    "{\"a\":\"x\",\"b\":2}",
    codegen_gate_io.ac_sparse_decoder(),
    codegen_gate_io.ac_sparse_to_json,
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

/// Scaffold → add coverage schemas → `aion codegen` → determinism →
/// `--check` clean → `gleam build` + runtime round-trip → drift fails
/// `--check` naming the file.
#[test]
fn codegen_generates_compiles_round_trips_and_checks() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let (project, _) = common::scaffold_project(temp_dir.path(), "codegen_gate", &[])?;
    std::fs::write(project.join("schemas/aa_shadow.json"), SHADOW_SCHEMA)?;
    std::fs::write(project.join("schemas/ab_empty.json"), EMPTY_SCHEMA)?;
    std::fs::write(project.join("schemas/ac_sparse.json"), SPARSE_SCHEMA)?;
    std::fs::write(project.join("schemas/demo_event.json"), DEMO_EVENT_SCHEMA)?;
    std::fs::write(project.join("schemas/zz_label.json"), LABEL_SCHEMA)?;

    // Generate.
    let output = common::run_cli(&project, &["codegen", "."])?;
    let report = common::success_json(&output)?;
    assert_eq!(report["module"], "src/codegen_gate_io.gleam");
    assert_eq!(report["action"], "written");
    assert_eq!(
        report["schemas"]
            .as_array()
            .ok_or("codegen report must list schemas")?
            .iter()
            .map(|value| value.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>(),
        vec![
            "schemas/aa_shadow.json",
            "schemas/ab_empty.json",
            "schemas/ac_sparse.json",
            "schemas/demo_event.json",
            "schemas/input.json",
            "schemas/output.json",
            "schemas/zz_label.json",
        ]
    );
    let module_path = project.join("src/codegen_gate_io.gleam");
    let first = std::fs::read_to_string(&module_path)?;
    assert!(
        first
            .starts_with("//// Generated by aion codegen — do not edit; regenerate from schemas/."),
        "generated module must carry the do-not-edit header; got:\n{first}"
    );

    // Determinism: regeneration is byte-identical.
    common::success_json(&common::run_cli(&project, &["codegen", "."])?)?;
    let second = std::fs::read_to_string(&module_path)?;
    assert_eq!(first, second, "regeneration must be byte-identical");

    // A clean tree passes --check.
    let checked = common::success_json(&common::run_cli(&project, &["codegen", ".", "--check"])?)?;
    assert_eq!(checked["action"], "checked");

    // The generated module compiles and round-trips under the real toolchain.
    common::patch_aion_flow_to_workspace(&project)?;
    std::fs::write(
        project.join("src/codegen_round_trip.gleam"),
        ROUND_TRIP_MODULE,
    )?;
    common::gleam_build(&project)?;
    gleam_run_round_trip(&project)?;

    // The project still packages with the generated module in its sources.
    common::package_project(&project, "codegen_gate")?;

    // Drift (a hand edit) fails --check, naming the file.
    let mut perturbed = first.clone();
    perturbed.push_str("\n// hand edit\n");
    std::fs::write(&module_path, &perturbed)?;
    let output = common::run_cli(&project, &["codegen", ".", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "--check drift must exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("codegen_gate_io.gleam") && stderr.contains("differs"),
        "--check failure must name the drifted file: {stderr}"
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
                "the codegen gate requires the `gleam` CLI on PATH (failed to spawn \
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

/// Real-world proof: the five stacked-dev schemas inside the v1 subset
/// (including `input.json` with its `repo_root`/caps fields) generate a module
/// that compiles under `gleam build`.
#[test]
fn stacked_dev_supported_schemas_generate_and_compile() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let project = temp_dir.path().join("stacked_dev_codegen");
    std::fs::create_dir_all(project.join("schemas"))?;
    std::fs::create_dir_all(project.join("src"))?;

    let example_schemas = repo_examples_dir()?.join("stacked-dev/schemas");
    // brief_dev_output.json is the inner-child output contract (scout/dev/review
    // blocks, all inlined within the v1 subset since the brief-dev migration).
    for name in [
        "input.json",
        "output.json",
        "gate_input.json",
        "gate_output.json",
        "brief_dev_output.json",
    ] {
        std::fs::copy(
            example_schemas.join(name),
            project.join("schemas").join(name),
        )?;
    }
    std::fs::write(
        project.join("gleam.toml"),
        "name = \"stacked_dev_codegen\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n\
         [dependencies]\ngleam_stdlib = \">= 0.60.0 and < 2.0.0\"\n\
         gleam_json = \">= 2.0.0 and < 4.0.0\"\n",
    )?;
    std::fs::write(
        project.join("workflow.toml"),
        "[[workflow]]\nentry_module = \"stacked_dev\"\nentry_function = \"run\"\n\
         timeout_seconds = 604800\ninput_schema = \"schemas/input.json\"\n\
         output_schema = \"schemas/output.json\"\nactivities = [\"land\"]\n\n\
         [[workflow]]\nentry_module = \"gate\"\nentry_function = \"run\"\n\
         timeout_seconds = 21600\ninput_schema = \"schemas/gate_input.json\"\n\
         output_schema = \"schemas/gate_output.json\"\nactivities = [\"full_checks\"]\n",
    )?;

    let output = common::run_cli(&project, &["codegen", "."])?;
    let report = common::success_json(&output)?;
    assert_eq!(report["module"], "src/stacked_dev_codegen_io.gleam");
    let module = std::fs::read_to_string(project.join("src/stacked_dev_codegen_io.gleam"))?;
    for expected in [
        "pub type Input {",
        "repo_root: String,",
        "verify_fix_cap: Int,",
        "review_deadline_ms: Int,",
        "pub type InputPlacement {",
        "pub type GateInputWorkspaceIsolation {",
        "pub type GateInputScope {",
        "modules: option.Option(List(String)),",
        "pub type BriefDevOutputDev {",
    ] {
        assert!(
            module.contains(expected),
            "generated stacked-dev module must contain `{expected}`; got:\n{module}"
        );
    }

    common::gleam_build(&project)?;
    Ok(())
}

/// Loud-failure proof: a schema that factors its shape through `$defs`/`$ref`
/// is outside the v1 codegen subset, so `aion codegen --check` exits 1 and
/// names the offending file and JSON pointer rather than emitting a partial
/// module. Held independent of any example — every stacked-dev schema is
/// in-subset since the brief-dev migration, so this builds its own fixture.
#[test]
fn schema_outside_subset_fails_loudly() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let project = temp_dir.path().join("outside_subset");
    std::fs::create_dir_all(project.join("schemas"))?;

    std::fs::write(
        project.join("schemas/factored.json"),
        r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["endpoint"],
  "additionalProperties": false,
  "$defs": {
    "host": { "type": "string" }
  },
  "properties": {
    "endpoint": { "$ref": "#/$defs/host" }
  }
}"##,
    )?;
    std::fs::write(
        project.join("gleam.toml"),
        "name = \"outside_subset\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n\
         [dependencies]\ngleam_stdlib = \">= 0.60.0 and < 2.0.0\"\n\
         gleam_json = \">= 2.0.0 and < 4.0.0\"\n",
    )?;
    std::fs::write(
        project.join("workflow.toml"),
        "[[workflow]]\nentry_module = \"outside_subset\"\nentry_function = \"run\"\n\
         timeout_seconds = 604800\ninput_schema = \"schemas/factored.json\"\n\
         output_schema = \"schemas/factored.json\"\nactivities = []\n",
    )?;

    let output = common::run_cli(&project, &["codegen", ".", "--check"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "out-of-subset schema must exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("schemas/factored.json")
            && stderr.contains("/$defs")
            && stderr.contains("unsupported JSON Schema construct"),
        "the loud error must name the file and pointer: {stderr}"
    );
    Ok(())
}

fn repo_examples_dir() -> Result<PathBuf, TestError> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()?)
}
