use std::{fs, path::PathBuf, time::Duration};

use serde_json::json;

use super::load_config;
use crate::project::{error::PackagingError, fixture};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const REQUIRED_LINES: [(&str, &str); 6] = [
    ("entry_module", r#"entry_module = "demo""#),
    ("entry_function", r#"entry_function = "run""#),
    ("timeout_seconds", "timeout_seconds = 30"),
    ("input_schema", r#"input_schema = "schemas/input.json""#),
    ("output_schema", r#"output_schema = "schemas/output.json""#),
    ("activities", r#"activities = ["greet"]"#),
];

fn workflow_block(omitted: Option<&str>) -> String {
    let mut text = String::from("[[workflow]]\n");
    for (field, line) in REQUIRED_LINES {
        if Some(field) != omitted {
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

fn descriptor_project(
    label: &str,
    descriptor: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    fixture::temp_project(
        label,
        &[
            ("workflow.toml", descriptor.as_bytes()),
            ("schemas/input.json", br#"{ "type": "object" }"#),
            ("schemas/output.json", b"true"),
        ],
    )
}

fn load_and_clean(
    label: &str,
    descriptor: &str,
) -> Result<(PathBuf, Result<super::ProjectConfig, PackagingError>), Box<dyn std::error::Error>> {
    let root = descriptor_project(label, descriptor)?;
    let result = load_config(&root);
    fs::remove_dir_all(&root)?;
    Ok((root, result))
}

#[test]
fn full_descriptor_round_trips_with_derived_and_explicit_outputs() -> TestResult {
    let descriptor = format!(
        "[package]\ninclude_source = false\n\n{}\n[[workflow]]\n\
         entry_module = \"demo@nested\"\nentry_function = \"start\"\n\
         timeout_seconds = 3600\ninput_schema = \"schemas/input.json\"\n\
         output_schema = \"schemas/output.json\"\nactivities = []\n\
         output = \"custom-name.aion\"\n",
        workflow_block(None)
    );
    let (root, result) = load_and_clean("config-full", &descriptor)?;
    let config = result?;

    assert!(!config.include_source);
    assert_eq!(config.workflows.len(), 2);
    let first = &config.workflows[0];
    assert_eq!(first.entry_module, "demo");
    assert_eq!(first.entry_function, "run");
    assert_eq!(first.timeout, Duration::from_secs(30));
    assert_eq!(first.input_schema, json!({ "type": "object" }));
    assert_eq!(first.output_schema, json!(true));
    assert_eq!(first.activities, vec!["greet".to_owned()]);
    assert_eq!(first.output_path, root.join("demo.aion"));
    let second = &config.workflows[1];
    assert_eq!(second.entry_module, "demo@nested");
    assert_eq!(second.timeout, Duration::from_secs(3600));
    assert!(second.activities.is_empty());
    assert_eq!(second.output_path, root.join("custom-name.aion"));
    Ok(())
}

#[test]
fn include_source_defaults_to_true_without_package_table() -> TestResult {
    let (_, result) = load_and_clean("config-default-source", &workflow_block(None))?;

    assert!(result?.include_source);
    Ok(())
}

#[test]
fn missing_descriptor_returns_config_missing() -> TestResult {
    let root = fixture::temp_project("config-missing", &[])?;
    let result = load_config(&root);
    fs::remove_dir_all(&root)?;

    assert!(matches!(
        result,
        Err(PackagingError::ConfigMissing { root: reported }) if reported == root
    ));
    Ok(())
}

#[test]
fn omitting_each_required_field_returns_config_parse_naming_it() -> TestResult {
    for (field, _) in REQUIRED_LINES {
        let label = format!("config-omit-{field}");
        let (_, result) = load_and_clean(&label, &workflow_block(Some(field)))?;

        let Err(PackagingError::ConfigParse { source, .. }) = result else {
            return Err(format!("omitting {field} did not produce ConfigParse").into());
        };
        assert!(
            source.to_string().contains(field),
            "parse error for omitted {field} does not name it: {source}"
        );
    }
    Ok(())
}

#[test]
fn unknown_keys_in_any_table_return_config_parse_naming_them() -> TestResult {
    let cases = [
        ("top", format!("mystery = 1\n{}", workflow_block(None))),
        (
            "package",
            format!("[package]\nmystery = true\n\n{}", workflow_block(None)),
        ),
        ("workflow", format!("{}mystery = 1\n", workflow_block(None))),
    ];
    for (table, descriptor) in cases {
        let label = format!("config-unknown-{table}");
        let (_, result) = load_and_clean(&label, &descriptor)?;

        let Err(PackagingError::ConfigParse { source, .. }) = result else {
            return Err(format!("unknown key in {table} did not produce ConfigParse").into());
        };
        assert!(
            source.to_string().contains("mystery"),
            "parse error for {table} does not name the unknown key: {source}"
        );
    }
    Ok(())
}

#[test]
fn zero_timeout_returns_config_invalid() -> TestResult {
    let descriptor = workflow_block(Some("timeout_seconds")) + "timeout_seconds = 0\n";
    let (_, result) = load_and_clean("config-zero-timeout", &descriptor)?;

    assert!(matches!(
        result,
        Err(PackagingError::ConfigInvalid { field, .. })
            if field == "workflow[0].timeout_seconds"
    ));
    Ok(())
}

#[test]
fn unsafe_entry_modules_return_config_invalid() -> TestResult {
    for (case, module) in [
        ("dollar", "demo$bad"),
        ("dotdot", "../escape"),
        ("empty", ""),
    ] {
        let descriptor =
            workflow_block(Some("entry_module")) + &format!("entry_module = \"{module}\"\n");
        let label = format!("config-unsafe-{case}");
        let (_, result) = load_and_clean(&label, &descriptor)?;

        assert!(
            matches!(
                result,
                Err(PackagingError::ConfigInvalid { ref field, .. })
                    if field == "workflow[0].entry_module"
            ),
            "entry module `{module}` was not rejected: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn empty_entry_function_returns_config_invalid() -> TestResult {
    let descriptor = workflow_block(Some("entry_function")) + "entry_function = \"\"\n";
    let (_, result) = load_and_clean("config-empty-function", &descriptor)?;

    assert!(matches!(
        result,
        Err(PackagingError::ConfigInvalid { field, .. })
            if field == "workflow[0].entry_function"
    ));
    Ok(())
}

#[test]
fn duplicate_entry_modules_return_config_invalid() -> TestResult {
    let descriptor = format!(
        "{}\n{}output = \"second.aion\"\n",
        workflow_block(None),
        workflow_block(None)
    );
    let (_, result) = load_and_clean("config-dup-modules", &descriptor)?;

    assert!(matches!(
        result,
        Err(PackagingError::ConfigInvalid { field, reason })
            if field == "workflow[1].entry_module" && reason.contains("workflow[0]")
    ));
    Ok(())
}

#[test]
fn absolute_schema_path_outside_root_is_rejected_unread() -> TestResult {
    let outside = std::env::temp_dir().join("aion-config-outside-secret.json");
    fs::write(&outside, br#"{ "type": "object" }"#)?;
    let outside_str = outside.to_str().ok_or("non-UTF-8 temp path")?.to_owned();
    let descriptor =
        workflow_block(Some("input_schema")) + &format!("input_schema = \"{outside_str}\"\n");
    let (_, result) = load_and_clean("config-abs-schema", &descriptor)?;
    fs::remove_file(&outside)?;

    assert!(
        matches!(
            result,
            Err(PackagingError::PathEscapesRoot { ref field, ref path })
                if field == "workflow[0].input_schema" && *path == outside
        ),
        "absolute schema path was not rejected: {result:?}"
    );
    Ok(())
}

#[test]
fn dotdot_output_escaping_root_is_rejected() -> TestResult {
    let descriptor = workflow_block(None) + "output = \"../../escape.aion\"\n";
    let (_, result) = load_and_clean("config-escape-output", &descriptor)?;

    assert!(
        matches!(
            result,
            Err(PackagingError::PathEscapesRoot { ref field, ref path })
                if field == "workflow[0].output"
                    && path == &PathBuf::from("../../escape.aion")
        ),
        "escaping output was not rejected: {result:?}"
    );
    Ok(())
}

#[test]
fn dotdot_output_schema_escaping_root_is_rejected() -> TestResult {
    let descriptor =
        workflow_block(Some("output_schema")) + "output_schema = \"schemas/../../outside.json\"\n";
    let (_, result) = load_and_clean("config-escape-output-schema", &descriptor)?;

    assert!(
        matches!(
            result,
            Err(PackagingError::PathEscapesRoot { ref field, ref path })
                if field == "workflow[0].output_schema"
                    && path == &PathBuf::from("schemas/../../outside.json")
        ),
        "escaping output_schema was not rejected: {result:?}"
    );
    Ok(())
}

#[test]
fn dotdot_paths_staying_inside_root_are_accepted_and_normalized() -> TestResult {
    let descriptor = workflow_block(Some("input_schema"))
        + "input_schema = \"sub/../schemas/input.json\"\noutput = \"sub/../demo.aion\"\n";
    let (root, result) = load_and_clean("config-inside-dotdot", &descriptor)?;
    let config = result?;

    assert_eq!(config.workflows[0].output_path, root.join("demo.aion"));
    assert_eq!(
        config.workflows[0].input_schema,
        json!({ "type": "object" })
    );
    Ok(())
}

#[test]
fn textually_distinct_outputs_naming_the_same_file_conflict() -> TestResult {
    let second = workflow_block(Some("entry_module"))
        + "entry_module = \"demo@nested\"\noutput = \"sub/../demo.aion\"\n";
    let descriptor = format!("{}output = \"demo.aion\"\n\n{second}", workflow_block(None));
    let (root, result) = load_and_clean("config-normalized-conflict", &descriptor)?;

    assert!(
        matches!(
            result,
            Err(PackagingError::OutputConflict { ref first, ref second, ref path })
                if first == "demo" && second == "demo@nested"
                    && *path == root.join("demo.aion")
        ),
        "normalized-equal outputs did not conflict: {result:?}"
    );
    Ok(())
}

#[test]
fn explicit_output_conflicts_are_rejected_with_both_workflows() -> TestResult {
    let second = workflow_block(Some("entry_module"))
        + "entry_module = \"demo@nested\"\noutput = \"demo.aion\"\n";
    let descriptor = format!("{}\n{second}", workflow_block(None));
    let (root, result) = load_and_clean("config-output-conflict", &descriptor)?;

    assert!(matches!(
        result,
        Err(PackagingError::OutputConflict { first, second, path })
            if first == "demo" && second == "demo@nested" && path == root.join("demo.aion")
    ));
    Ok(())
}

#[test]
fn duplicate_activities_return_config_invalid() -> TestResult {
    let descriptor = workflow_block(Some("activities")) + "activities = [\"greet\", \"greet\"]\n";
    let (_, result) = load_and_clean("config-dup-activities", &descriptor)?;

    assert!(matches!(
        result,
        Err(PackagingError::ConfigInvalid { field, reason })
            if field == "workflow[0].activities" && reason.contains("greet")
    ));
    Ok(())
}

#[test]
fn empty_activity_strings_return_config_invalid() -> TestResult {
    let descriptor = workflow_block(Some("activities")) + "activities = [\"\"]\n";
    let (_, result) = load_and_clean("config-empty-activity", &descriptor)?;

    assert!(matches!(
        result,
        Err(PackagingError::ConfigInvalid { field, .. })
            if field == "workflow[0].activities"
    ));
    Ok(())
}

#[test]
fn zero_workflow_tables_return_config_invalid() -> TestResult {
    for (case, descriptor) in [("empty", ""), ("package-only", "[package]\n")] {
        let label = format!("config-no-workflows-{case}");
        let (_, result) = load_and_clean(&label, descriptor)?;

        assert!(
            matches!(
                result,
                Err(PackagingError::ConfigInvalid { ref field, .. }) if field == "workflow"
            ),
            "{case} descriptor was not rejected: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn missing_schema_file_returns_actionable_schema_missing() -> TestResult {
    let root = fixture::temp_project(
        "config-schema-missing",
        &[
            ("workflow.toml", workflow_block(None).as_bytes()),
            ("schemas/output.json", b"true"),
        ],
    )?;
    let result = load_config(&root);
    fs::remove_dir_all(&root)?;

    let Err(PackagingError::SchemaMissing { path }) = result else {
        return Err(format!("expected SchemaMissing, got {result:?}").into());
    };
    assert_eq!(path, root.join("schemas/input.json"));
    // The error must point at generation, not at restoring a hand file.
    let message = PackagingError::SchemaMissing { path }.to_string();
    assert!(
        message.contains("aion generate") && message.contains("generated artifacts"),
        "missing-schema error must be actionable: {message}"
    );
    Ok(())
}

#[test]
fn invalid_schema_json_returns_schema_parse_with_path() -> TestResult {
    let root = fixture::temp_project(
        "config-schema-invalid",
        &[
            ("workflow.toml", workflow_block(None).as_bytes()),
            ("schemas/input.json", b"{ not json"),
            ("schemas/output.json", b"true"),
        ],
    )?;
    let result = load_config(&root);
    fs::remove_dir_all(&root)?;

    assert!(matches!(
        result,
        Err(PackagingError::SchemaParse { path, .. })
            if path == root.join("schemas/input.json")
    ));
    Ok(())
}

#[test]
fn generated_awl_sidecar_supplies_additional_workflow_entries() -> TestResult {
    let root = descriptor_project("config-awl-sidecar", &workflow_block(None))?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src/demo.awl.json"),
        serde_json::to_vec_pretty(&json!({
            "format_version": 1,
            "entry_module": "demo",
            "synthesized_workflows": [{
                "workflow_type": "aion_internal_awl_child_demo_fan_0",
                "entry_module": "demo",
                "entry_function": "aion_internal_awl_child_demo_fan_0_run",
                "timeout_seconds": 30,
                "input_schema": {"type": "object"},
                "output_schema": {"type": "string"},
                "internal": true
            }]
        }))?,
    )?;
    let config = load_config(&root)?;
    fs::remove_dir_all(&root)?;
    let entries = &config.workflows[0].additional_workflows;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].workflow_type,
        "aion_internal_awl_child_demo_fan_0"
    );
    assert_eq!(entries[0].output_schema, json!({"type": "string"}));
    assert!(entries[0].internal);
    Ok(())
}

#[test]
fn workflow_types_must_be_unique_across_every_project_entry() -> TestResult {
    let descriptor = format!(
        "{}\n[[workflow]]\nentry_module = \"other\"\nentry_function = \"run\"\n\
         timeout_seconds = 30\ninput_schema = \"schemas/input.json\"\n\
         output_schema = \"schemas/output.json\"\nactivities = []\n",
        workflow_block(None)
    );
    let root = descriptor_project("config-global-type-collision", &descriptor)?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src/demo.awl.json"),
        serde_json::to_vec(&json!({
            "format_version": 1,
            "entry_module": "demo",
            "synthesized_workflows": [{
                "workflow_type": "other",
                "entry_module": "demo",
                "entry_function": "child_run",
                "timeout_seconds": 30,
                "input_schema": {},
                "output_schema": {},
                "internal": true
            }]
        }))?,
    )?;
    let result = load_config(&root);
    fs::remove_dir_all(&root)?;
    assert!(
        matches!(result, Err(PackagingError::ConfigInvalid { reason, .. })
            if reason.contains("workflow type `other`") && reason.contains("duplicates"))
    );
    Ok(())
}
