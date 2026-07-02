//! Semantic-equivalence regression pin for the types-first migration
//! (ADR-014): the schemas EMITTED from `examples/order-saga`'s authored types
//! module must be semantically equivalent to the schemas that used to be
//! AUTHORED before the migration — same types, same `required` sets, same
//! wire property names, same `additionalProperties: false` closure — so the
//! packaging boundary and `aion input` behave identically pre/post migration.
//!
//! The previously-authored schema documents are embedded verbatim below as
//! the fixed reference. Two deliberate, documented differences are excluded
//! from the comparison: the `$schema`/`$comment` annotation keys, and the
//! validation keywords (`minLength`, `minimum`) that a Gleam type cannot
//! express — the migration dropped those constraints, and this pin makes that
//! drop explicit rather than silent.
//!
//! The model is built through the REAL pipeline (`gleam export
//! package-interface` on the staged example → interface front end → schema
//! emitter). Per project policy this never uses `#[ignore]`: without a gleam
//! toolchain it logs a skip and passes; with one it runs for real.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_package::{CodegenMode, boundary_types_from_interface, emit_schemas};
use serde_json::Value;

type TestError = Box<dyn std::error::Error>;

/// The eight order-saga schemas exactly as they were authored before the
/// types-first migration, keyed by emitted file name.
const AUTHORED: &[(&str, &str)] = &[
    (
        "cancel_shipment_input.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["order_id", "shipment_id"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "shipment_id": { "type": "string" }
  }
}"#,
    ),
    (
        "compensation_output.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["status", "detail"],
  "additionalProperties": false,
  "properties": {
    "status": { "type": "string" },
    "detail": { "type": "string" }
  }
}"#,
    ),
    (
        "inventory_reservation.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["order_id", "reservation_id", "item", "quantity"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "reservation_id": { "type": "string" },
    "item": { "type": "string" },
    "quantity": { "type": "integer" }
  }
}"#,
    ),
    (
        "order_input.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["order_id", "item", "quantity", "amount"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string", "minLength": 1 },
    "item": { "type": "string", "minLength": 1 },
    "quantity": { "type": "integer", "minimum": 1 },
    "amount": { "type": "integer", "minimum": 1 }
  }
}"#,
    ),
    (
        "payment_receipt.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["order_id", "payment_id", "amount"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "payment_id": { "type": "string" },
    "amount": { "type": "integer" }
  }
}"#,
    ),
    (
        "refund_payment_input.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["order_id", "payment_id", "amount"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "payment_id": { "type": "string" },
    "amount": { "type": "integer" }
  }
}"#,
    ),
    (
        "release_inventory_input.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["order_id", "reservation_id", "item", "quantity"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "reservation_id": { "type": "string" },
    "item": { "type": "string" },
    "quantity": { "type": "integer" }
  }
}"#,
    ),
    (
        "shipment.json",
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["order_id", "shipment_id"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "shipment_id": { "type": "string" }
  }
}"#,
    ),
];

/// Annotation and type-inexpressible validation keywords excluded from the
/// semantic comparison, by design (documented in docs/guides/codegen.md).
const EXCLUDED_KEYS: &[&str] = &["$schema", "$comment", "minLength", "minimum", "minItems"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn gleam_available() -> bool {
    Command::new("gleam")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Copies `examples/order-saga` into a temp dir with the `aion_flow` path
/// dependency rewritten to an absolute path (mirrors the other gates).
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

/// Strips the excluded annotation/validation keys recursively, leaving only
/// the semantic shape (type / required / properties / additionalProperties /
/// items / enum).
fn semantic_shape(value: &Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .filter(|(key, _)| !EXCLUDED_KEYS.contains(&key.as_str()))
                .map(|(key, child)| (key.clone(), semantic_shape(child)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(semantic_shape).collect()),
        other => other.clone(),
    }
}

#[test]
fn emitted_order_saga_schemas_are_semantically_equivalent_to_the_authored_originals()
-> Result<(), TestError> {
    if !gleam_available() {
        eprintln!("skipping schema_equivalence: gleam toolchain not on PATH");
        return Ok(());
    }
    let (_temp, project) = stage_order_saga()?;

    // Export the interface of the committed example (it compiles as-is) and
    // run the real front end + schema emitter.
    let out = project.join("build/equivalence-interface.json");
    let output = Command::new("gleam")
        .args(["export", "package-interface", "--out"])
        .arg(&out)
        .current_dir(&project)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "`gleam export package-interface` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let interface = fs::read(&out)?;
    let types = boundary_types_from_interface(&interface, "aion_order_saga")?;

    // The committed emitted schemas must already be byte-current (check mode
    // proves determinism against the repository state)…
    let report = emit_schemas(&project, "aion_order_saga", &types, CodegenMode::Check)?;
    let emitted: BTreeSet<&str> = report.emitted.iter().map(String::as_str).collect();
    let expected: BTreeSet<String> = AUTHORED
        .iter()
        .map(|(name, _)| format!("schemas/{name}"))
        .collect();
    let expected_refs: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    assert_eq!(
        emitted, expected_refs,
        "the emitted schema set must cover exactly the previously-authored set"
    );

    // …and each emitted document must match its previously-authored original
    // semantically: same type/required/wire-names/closure, with only the
    // documented annotation/validation keys differing.
    for (name, authored) in AUTHORED {
        let emitted_doc: Value =
            serde_json::from_slice(&fs::read(project.join("schemas").join(name))?)?;
        let authored_doc: Value = serde_json::from_str(authored)?;
        assert_eq!(
            semantic_shape(&emitted_doc),
            semantic_shape(&authored_doc),
            "schemas/{name} drifted semantically from the pre-migration authored document"
        );
    }
    Ok(())
}
