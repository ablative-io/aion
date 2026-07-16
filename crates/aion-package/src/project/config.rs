//! `workflow.toml` descriptor parsing, semantic validation, and schema loading.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;

use super::{confine::resolve_confined, error::PackagingError};
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
    #[serde(default)]
    additional_workflows: Vec<RawAdditionalWorkflow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAdditionalWorkflow {
    workflow_type: String,
    entry_module: String,
    entry_function: String,
    timeout_seconds: u64,
    input_schema: String,
    output_schema: String,
    #[serde(default)]
    internal: bool,
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
    /// Input schema path resolved against the project root.
    pub(crate) input_schema_path: PathBuf,
    /// Output schema path resolved against the project root.
    pub(crate) output_schema_path: PathBuf,
    /// Declared activity types, validated non-empty and unique.
    pub(crate) activities: Vec<String>,
    /// Additional same-archive workflow entries.
    pub(crate) additional_workflows: Vec<AdditionalWorkflowConfig>,
    /// Archive output path resolved against the project root.
    pub(crate) output_path: PathBuf,
}

/// One validated same-archive workflow entry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AdditionalWorkflowConfig {
    pub(crate) workflow_type: String,
    pub(crate) entry_module: String,
    pub(crate) entry_function: String,
    pub(crate) timeout: Duration,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) output_schema: serde_json::Value,
    pub(crate) internal: bool,
}

/// Loads and validates `<root>/workflow.toml`, resolving all declared paths
/// against `root` and parsing the declared schema files.
///
/// Declared paths (`output`, `input_schema`, `output_schema`) are lexically
/// normalized and must resolve inside `root`; absolute paths and `..`
/// traversal that escapes the root are rejected with
/// [`PackagingError::PathEscapesRoot`].
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

/// Reads and parses one declared JSON-Schema file. A missing file gets the
/// actionable [`PackagingError::SchemaMissing`] — schemas are generated
/// artifacts, so the fix on a fresh clone is `aion generate`, not restoring a
/// hand-authored file.
pub(crate) fn load_schema(path: &Path) -> Result<serde_json::Value, PackagingError> {
    let bytes = fs::read(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            PackagingError::SchemaMissing {
                path: path.to_path_buf(),
            }
        } else {
            PackagingError::SchemaRead {
                path: path.to_path_buf(),
                source,
            }
        }
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

    let mut workflow_types = BTreeMap::new();
    workflow_types.insert(entry.entry_module.as_str(), 0usize);
    for (position, additional) in entry.additional_workflows.iter().enumerate() {
        let field =
            |name: &str| format!("workflow[{index}].additional_workflows[{position}].{name}");
        if additional.workflow_type.is_empty() {
            return Err(PackagingError::ConfigInvalid {
                field: field("workflow_type"),
                reason: "must not be empty".to_owned(),
            });
        }
        if !is_safe_logical_name(&additional.workflow_type)
            || !is_safe_logical_name(&additional.entry_module)
        {
            return Err(PackagingError::ConfigInvalid {
                field: field("workflow_type/entry_module"),
                reason: "must be safe logical names".to_owned(),
            });
        }
        if additional.entry_function.is_empty() || additional.timeout_seconds == 0 {
            return Err(PackagingError::ConfigInvalid {
                field: field("entry_function/timeout_seconds"),
                reason: "entry function must be non-empty and timeout at least 1".to_owned(),
            });
        }
        if let Some(first) = workflow_types.insert(additional.workflow_type.as_str(), position + 1)
        {
            return Err(PackagingError::ConfigInvalid {
                field: field("workflow_type"),
                reason: format!("duplicates workflow entry position {first}"),
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

/// Resolves output and schema paths against `root` — confining every
/// descriptor-declared path to the root — and rejects output conflicts.
///
/// Conflict detection runs on the normalized resolved paths, so textually
/// different spellings of the same file (`out.aion` vs `sub/../out.aion`)
/// are caught as the conflict they are on disk.
fn resolve_workflows(
    root: &Path,
    workflows: Vec<RawWorkflow>,
) -> Result<Vec<WorkflowConfig>, PackagingError> {
    let mut claimed_outputs: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut resolved = Vec::with_capacity(workflows.len());

    for (index, entry) in workflows.iter().enumerate() {
        let output_path = match &entry.output {
            Some(output) => resolve_confined(root, format!("workflow[{index}].output"), output)?,
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
        .enumerate()
        .map(|(index, (entry, output_path))| {
            let input_schema_path = resolve_confined(
                root,
                format!("workflow[{index}].input_schema"),
                &entry.input_schema,
            )?;
            let output_schema_path = resolve_confined(
                root,
                format!("workflow[{index}].output_schema"),
                &entry.output_schema,
            )?;
            let additional_workflows =
                resolve_additional(root, index, &entry.additional_workflows)?;
            Ok(WorkflowConfig {
                input_schema: load_schema(&input_schema_path)?,
                output_schema: load_schema(&output_schema_path)?,
                input_schema_path,
                output_schema_path,
                entry_module: entry.entry_module,
                entry_function: entry.entry_function,
                timeout: Duration::from_secs(entry.timeout_seconds),
                activities: entry.activities,
                additional_workflows,
                output_path,
            })
        })
        .collect()
}

fn resolve_additional(
    root: &Path,
    workflow_index: usize,
    entries: &[RawAdditionalWorkflow],
) -> Result<Vec<AdditionalWorkflowConfig>, PackagingError> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let prefix = format!("workflow[{workflow_index}].additional_workflows[{index}]");
            let input_path =
                resolve_confined(root, format!("{prefix}.input_schema"), &entry.input_schema)?;
            let output_path = resolve_confined(
                root,
                format!("{prefix}.output_schema"),
                &entry.output_schema,
            )?;
            Ok(AdditionalWorkflowConfig {
                workflow_type: entry.workflow_type.clone(),
                entry_module: entry.entry_module.clone(),
                entry_function: entry.entry_function.clone(),
                timeout: Duration::from_secs(entry.timeout_seconds),
                input_schema: load_schema(&input_path)?,
                output_schema: load_schema(&output_path)?,
                internal: entry.internal,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
