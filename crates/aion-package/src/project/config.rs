//! `workflow.toml` descriptor parsing, semantic validation, and schema loading.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;

use super::error::PackagingError;
use crate::builder::is_safe_logical_name;

/// File name of the workflow packaging descriptor inside the project root.
pub(crate) const CONFIG_FILE_NAME: &str = "workflow.toml";

/// File extension of produced workflow package archives.
const ARCHIVE_EXTENSION: &str = "aion";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    package: Option<RawPackage>,
    #[serde(default)]
    workflow: Vec<RawWorkflow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    include_source: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkflow {
    entry_module: String,
    entry_function: String,
    timeout_seconds: u64,
    input_schema: String,
    output_schema: String,
    activities: Vec<String>,
    output: Option<String>,
}

/// Validated packaging configuration loaded from a project's `workflow.toml`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectConfig {
    /// Whether first-party `src/**/*.gleam` files ship inside the archives.
    pub(crate) include_source: bool,
    /// Validated `[[workflow]]` entries in declaration order.
    pub(crate) workflows: Vec<WorkflowConfig>,
}

/// One validated `[[workflow]]` entry with resolved paths and loaded schemas.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkflowConfig {
    /// Logical entry module; also the workflow type.
    pub(crate) entry_module: String,
    /// Exported entry function on the entry module.
    pub(crate) entry_function: String,
    /// Workflow timeout declared in whole seconds.
    pub(crate) timeout: Duration,
    /// Parsed JSON-Schema document for input payloads.
    pub(crate) input_schema: serde_json::Value,
    /// Parsed JSON-Schema document for result payloads.
    pub(crate) output_schema: serde_json::Value,
    /// Declared activity types, validated non-empty and unique.
    pub(crate) activities: Vec<String>,
    /// Archive output path resolved against the project root.
    pub(crate) output_path: PathBuf,
}

/// Loads and validates `<root>/workflow.toml`, resolving all relative paths
/// against `root` and parsing the declared schema files.
pub(crate) fn load_config(root: &Path) -> Result<ProjectConfig, PackagingError> {
    let path = root.join(CONFIG_FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(PackagingError::ConfigMissing {
                root: root.to_path_buf(),
            });
        }
        Err(source) => return Err(PackagingError::ConfigRead { path, source }),
    };
    let raw: RawConfig =
        toml::from_str(&text).map_err(|source| PackagingError::ConfigParse { path, source })?;
    validate(root, raw)
}

/// Reads and parses one declared JSON-Schema file.
pub(crate) fn load_schema(path: &Path) -> Result<serde_json::Value, PackagingError> {
    let bytes = fs::read(path).map_err(|source| PackagingError::SchemaRead {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| PackagingError::SchemaParse {
        path: path.to_path_buf(),
        source,
    })
}

fn validate(root: &Path, raw: RawConfig) -> Result<ProjectConfig, PackagingError> {
    if raw.workflow.is_empty() {
        return Err(PackagingError::ConfigInvalid {
            field: "workflow".to_owned(),
            reason: "at least one [[workflow]] entry is required".to_owned(),
        });
    }

    for (index, entry) in raw.workflow.iter().enumerate() {
        validate_fields(index, entry)?;
    }
    validate_unique_entry_modules(&raw.workflow)?;

    let include_source = raw
        .package
        .and_then(|package| package.include_source)
        .unwrap_or(true);
    let workflows = resolve_workflows(root, raw.workflow)?;

    Ok(ProjectConfig {
        include_source,
        workflows,
    })
}

fn validate_fields(index: usize, entry: &RawWorkflow) -> Result<(), PackagingError> {
    let invalid = |field: &str, reason: &str| PackagingError::ConfigInvalid {
        field: format!("workflow[{index}].{field}"),
        reason: reason.to_owned(),
    };

    if entry.entry_module.is_empty() {
        return Err(invalid("entry_module", "must not be empty"));
    }
    if !is_safe_logical_name(&entry.entry_module) {
        return Err(invalid(
            "entry_module",
            "is not a safe logical module name (no `$`, backslashes, leading separators, \
             or empty/`.`/`..` path components)",
        ));
    }
    if entry.entry_function.is_empty() {
        return Err(invalid("entry_function", "must not be empty"));
    }
    if entry.timeout_seconds == 0 {
        return Err(invalid(
            "timeout_seconds",
            "must be an integer of at least 1",
        ));
    }

    let mut seen = BTreeMap::new();
    for (position, activity) in entry.activities.iter().enumerate() {
        if activity.is_empty() {
            return Err(invalid("activities", "must not contain empty strings"));
        }
        if let Some(first) = seen.insert(activity.as_str(), position) {
            return Err(PackagingError::ConfigInvalid {
                field: format!("workflow[{index}].activities"),
                reason: format!(
                    "activity `{activity}` is declared more than once \
                     (positions {first} and {position})"
                ),
            });
        }
    }

    Ok(())
}

fn validate_unique_entry_modules(workflows: &[RawWorkflow]) -> Result<(), PackagingError> {
    let mut seen = BTreeMap::new();
    for (index, entry) in workflows.iter().enumerate() {
        if let Some(first) = seen.insert(entry.entry_module.as_str(), index) {
            return Err(PackagingError::ConfigInvalid {
                field: format!("workflow[{index}].entry_module"),
                reason: format!(
                    "duplicates the entry module `{}` of workflow[{first}]",
                    entry.entry_module
                ),
            });
        }
    }
    Ok(())
}

fn resolve_workflows(
    root: &Path,
    workflows: Vec<RawWorkflow>,
) -> Result<Vec<WorkflowConfig>, PackagingError> {
    let mut claimed_outputs: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut resolved = Vec::with_capacity(workflows.len());

    for entry in &workflows {
        let output_path = match &entry.output {
            Some(output) => root.join(output),
            None => root.join(format!("{}.{ARCHIVE_EXTENSION}", entry.entry_module)),
        };
        if let Some(first) = claimed_outputs.insert(output_path.clone(), entry.entry_module.clone())
        {
            return Err(PackagingError::OutputConflict {
                first,
                second: entry.entry_module.clone(),
                path: output_path,
            });
        }
        resolved.push(output_path);
    }

    workflows
        .into_iter()
        .zip(resolved)
        .map(|(entry, output_path)| {
            Ok(WorkflowConfig {
                input_schema: load_schema(&root.join(&entry.input_schema))?,
                output_schema: load_schema(&root.join(&entry.output_schema))?,
                entry_module: entry.entry_module,
                entry_function: entry.entry_function,
                timeout: Duration::from_secs(entry.timeout_seconds),
                activities: entry.activities,
                output_path,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
    ) -> Result<(PathBuf, Result<super::ProjectConfig, PackagingError>), Box<dyn std::error::Error>>
    {
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
        let descriptor =
            workflow_block(Some("activities")) + "activities = [\"greet\", \"greet\"]\n";
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
    fn missing_schema_file_returns_schema_read_with_path() -> TestResult {
        let root = fixture::temp_project(
            "config-schema-missing",
            &[
                ("workflow.toml", workflow_block(None).as_bytes()),
                ("schemas/output.json", b"true"),
            ],
        )?;
        let result = load_config(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::SchemaRead { path, .. })
                if path == root.join("schemas/input.json")
        ));
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
}
