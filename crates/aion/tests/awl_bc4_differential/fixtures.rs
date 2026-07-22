//! Fixture loading for the differential: the covered ratchet path list, the
//! rev-2 fixture tree, and a deterministic schema-driven input generator so
//! every fixture starts with a payload its own generated input codec accepts.

use std::fs;
use std::path::PathBuf;

use aion_awl::Document;
use serde_json::{Map, Value, json};

/// `<repo>/crates/aion-awl` — the crate whose `tests/fixtures/rev2` tree and
/// `src/mir/covered.rs` ratchet the differential reads.
pub fn awl_crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../aion-awl")
}

/// The rev-2 fixture root.
pub fn rev2_dir() -> PathBuf {
    awl_crate_dir().join("tests/fixtures/rev2")
}

/// Absolute path of a fixture given its covered-ratchet relative path.
pub fn fixture_path(relative: &str) -> PathBuf {
    rev2_dir().join(format!("{relative}.awl"))
}

/// A parsed fixture: its ratchet name, source, parsed document, and directory
/// (schema-door imports resolve against the directory).
pub struct Loaded {
    /// Covered-ratchet path, relative to `tests/fixtures/rev2`, no extension.
    pub name: String,
    /// Parsed document.
    pub document: Document,
    /// Directory the fixture lives in (import root).
    pub dir: PathBuf,
}

/// Reads and parses one covered fixture.
///
/// # Errors
///
/// Fails when the fixture cannot be read or does not parse.
pub fn load(relative: &str) -> Result<Loaded, Box<dyn std::error::Error>> {
    let path = fixture_path(relative);
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read fixture {relative}: {error}"))?;
    let document = aion_awl::parse(&source)
        .map_err(|error| format!("fixture {relative} does not parse: {error}"))?;
    let dir = path
        .parent()
        .ok_or_else(|| format!("fixture {relative} has no parent directory"))?
        .to_path_buf();
    Ok(Loaded {
        name: relative.to_owned(),
        document,
        dir,
    })
}

/// Parses the covered ratchet (`aion-awl/src/mir/covered.rs`) directly from
/// source so the differential fixture set stays in lockstep with the lowering
/// ratchet — a fixture added or removed there flows here with no second edit.
///
/// # Errors
///
/// Fails when the ratchet file cannot be read.
pub fn covered_paths() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let path = awl_crate_dir().join("src/mir/covered.rs");
    let text =
        fs::read_to_string(&path).map_err(|error| format!("cannot read covered.rs: {error}"))?;
    Ok(quoted_strings(&text)
        .into_iter()
        .filter(|entry| entry.contains("/valid/"))
        .collect())
}

/// Extracts every double-quoted string literal from `text`. The ratchet file
/// contains only path literals and (backtick-only) doc comments, so a simple
/// scan is exact.
fn quoted_strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character == '"' {
            let mut literal = String::new();
            for inner in chars.by_ref() {
                if inner == '"' {
                    break;
                }
                literal.push(inner);
            }
            out.push(literal);
        }
    }
    out
}

/// Builds a deterministic, minimal JSON value that satisfies `schema` (a
/// draft-2020-12 schema as produced by the AWL schema deriver). Required
/// object properties are populated; optional ones are omitted; arrays are
/// empty; enums take their first variant. The value only needs to satisfy the
/// generated input codec so the workflow can start and reach its body.
pub fn example_for_schema(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return Value::Null;
    };
    if let Some(Value::Array(variants)) = map.get("enum") {
        return variants.first().cloned().unwrap_or(Value::Null);
    }
    if let Some(Value::Array(branches)) = map.get("anyOf") {
        // Optionals lower to `anyOf: [T, null]`; the null branch is the
        // simplest satisfying value.
        if branches.iter().any(is_null_schema) {
            return Value::Null;
        }
        return branches.first().map_or(Value::Null, example_for_schema);
    }
    match map.get("type").and_then(Value::as_str) {
        Some("object") => example_object(map),
        Some("array") => Value::Array(Vec::new()),
        Some("string") => Value::String(String::from("x")),
        Some("integer") => json!(0),
        Some("number") => json!(0.0),
        Some("boolean") => Value::Bool(false),
        // `"null"` and an untyped `{}` schema (AWL `Unknown`, accepts
        // anything) both take the simplest satisfying value.
        _ => Value::Null,
    }
}

/// Whether a schema branch is exactly the `null` type.
fn is_null_schema(schema: &Value) -> bool {
    schema
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|ty| ty == "null")
}

/// Generates an object instance carrying only its required properties.
fn example_object(map: &Map<String, Value>) -> Value {
    let mut instance = Map::new();
    let required: Vec<&str> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(Value::Object(properties)) = map.get("properties") {
        for (name, property_schema) in properties {
            if required.contains(&name.as_str()) {
                instance.insert(name.clone(), example_for_schema(property_schema));
            }
        }
    }
    Value::Object(instance)
}
