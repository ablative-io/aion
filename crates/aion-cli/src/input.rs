//! Local `input` subcommand: a type-derived workflow input skeleton.
//!
//! `aion input <workflow_type>` emits a structurally-valid JSON input skeleton
//! derived from the workflow's input type — the `input_schema` its
//! `workflow.toml` declares, which is the same JSON-Schema document the input
//! codec is generated from. The skeleton therefore decodes through that codec
//! without a decode error, and is generated from the type rather than
//! hand-written (C30, S14).
//!
//! It carries no invented defaults (ADR-001): every required property appears
//! with a type-shaped placeholder the author replaces, and every optional
//! property is omitted. The skeleton derivation itself lives in
//! `aion_package::build_input_skeleton`; this command is the thin local driver
//! that resolves the workflow's input schema from the project and prints the
//! result.

use std::fs;
use std::path::{Path, PathBuf};

use aion_package::build_input_skeleton;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use toml_edit::DocumentMut;

/// Runs the `input` subcommand: emits the input skeleton for `workflow_type`
/// (the entry module name) in the project at `path`.
///
/// Returns the skeleton JSON document; the CLI prints it as the command result.
pub(crate) fn run(path: &Path, workflow_type: &str) -> Result<Value> {
    let schema_path = resolve_input_schema_path(path, workflow_type)?;
    let bytes = fs::read(&schema_path)
        .with_context(|| format!("failed to read input schema {}", schema_path.display()))?;
    let schema: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("input schema {} is not valid JSON", schema_path.display()))?;

    let skeleton = build_input_skeleton(&schema_path, &schema).with_context(|| {
        format!(
            "failed to derive an input skeleton from {}",
            schema_path.display()
        )
    })?;
    Ok(skeleton)
}

/// Resolves the `input_schema` path for the `[[workflow]]` whose `entry_module`
/// equals `workflow_type`, from the project's `workflow.toml`.
///
/// A single-workflow project may omit the type to take its sole workflow; a
/// multi-workflow project requires the type to match exactly, and a no-match is
/// a loud error naming the available workflow types rather than guessing.
fn resolve_input_schema_path(root: &Path, workflow_type: &str) -> Result<PathBuf> {
    let toml_path = root.join("workflow.toml");
    let contents = fs::read_to_string(&toml_path)
        .with_context(|| format!("failed to read {}", toml_path.display()))?;
    let document = contents
        .parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", toml_path.display()))?;
    let workflows = document
        .get("workflow")
        .and_then(|item| item.as_array_of_tables())
        .with_context(|| {
            format!(
                "{} declares no [[workflow]] entry to derive an input skeleton from",
                toml_path.display()
            )
        })?;

    let mut available: Vec<String> = Vec::new();
    let mut matched: Option<String> = None;
    for table in workflows {
        let Some(entry_module) = table.get("entry_module").and_then(|item| item.as_str()) else {
            continue;
        };
        available.push(entry_module.to_owned());
        if entry_module == workflow_type {
            let schema = table
                .get("input_schema")
                .and_then(|item| item.as_str())
                .with_context(|| {
                    format!(
                        "{} [[workflow]] `{entry_module}` declares no `input_schema`",
                        toml_path.display()
                    )
                })?;
            matched = Some(schema.to_owned());
        }
    }

    let Some(schema) = matched else {
        bail!(
            "no workflow `{workflow_type}` in {}; available workflow types: {}",
            toml_path.display(),
            available.join(", ")
        );
    };
    Ok(root.join(schema))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const WORKFLOW_TOML: &str = "[[workflow]]\n\
         entry_module = \"demo\"\n\
         entry_function = \"run\"\n\
         input_schema = \"schemas/input.json\"\n\
         output_schema = \"schemas/output.json\"\n\
         activities = []\n";

    const INPUT_SCHEMA: &[u8] = br#"{
        "type": "object",
        "required": ["name", "count"],
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "count": { "type": "integer" },
            "note": { "type": "string" }
        }
    }"#;

    fn project(label: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("aion-input-{label}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("schemas"))?;
        fs::write(root.join("workflow.toml"), WORKFLOW_TOML)?;
        fs::write(root.join("schemas/input.json"), INPUT_SCHEMA)?;
        Ok(root)
    }

    #[test]
    fn emits_type_derived_skeleton_for_the_named_workflow() -> TestResult {
        let root = project("named")?;
        let skeleton = run(&root, "demo")?;
        // Required fields present with type-shaped placeholders; the optional
        // `note` omitted (no invented default).
        assert_eq!(skeleton, serde_json::json!({ "name": "", "count": 0 }));
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn unknown_workflow_type_fails_naming_the_available_types() -> TestResult {
        let root = project("unknown")?;
        let result = run(&root, "nope");
        let Err(error) = result else {
            fs::remove_dir_all(&root)?;
            return Err("an unknown workflow type must fail".into());
        };
        let message = format!("{error}");
        assert!(message.contains("nope"), "{message}");
        assert!(
            message.contains("demo"),
            "must list available types: {message}"
        );
        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
