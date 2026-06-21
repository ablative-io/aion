//! Project-level codec generation: `workflow.toml` + `schemas/*.json` →
//! `src/<package>_io.gleam`.
//!
//! Generation is deterministic: schema files are processed in filename
//! (byte) order and property order is preserved from each JSON document, so
//! the same schemas always produce a byte-identical module. The module is
//! written only after every schema generated successfully — a loud error
//! never leaves a partial file behind.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::emit;
use super::error::CodegenError;
use super::json;
use super::names::{NameRegistry, is_reserved_word, is_snake_identifier};
use super::schema::{self, SchemaArtifact};
use crate::PackagingError;
use crate::project::config;

/// Directory (relative to the project root) codegen reads schemas from.
const SCHEMAS_DIR: &str = "schemas";

/// What to do with the generated module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodegenMode {
    /// Write `src/<package>_io.gleam`, replacing any existing file.
    Write,
    /// Compare against the on-disk file and fail on drift without writing
    /// (CI gate).
    Check,
}

/// Result of a successful codegen run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodegenReport {
    /// Absolute path of the generated module.
    pub module_path: PathBuf,
    /// Module path relative to the project root (`src/<package>_io.gleam`).
    pub module_relative: String,
    /// Schema files generated from, relative to the project root, in
    /// generation order.
    pub schemas: Vec<String>,
    /// The complete generated module source.
    pub contents: String,
    /// Whether the module was written (`false` in check mode).
    pub written: bool,
}

/// Generates Gleam types and JSON codecs for every `schemas/*.json` of the
/// workflow project at `root`, writing or checking
/// `src/<package>_io.gleam` per `mode`.
///
/// The project's `workflow.toml` is validated first (including that every
/// referenced schema exists, parses, and lives under `schemas/`), so codecs
/// can never be generated from schemas the packaging boundary would reject.
///
/// # Errors
///
/// Returns a [`CodegenError`] naming the offending file — and, for schema
/// constructs outside the supported subset, the JSON pointer — for: invalid
/// or missing `workflow.toml` / `gleam.toml`, missing or unreadable schema
/// files, invalid JSON, unsupported schema constructs, generated-name
/// collisions, write failures, and `--check` drift.
pub fn codegen_project(root: &Path, mode: CodegenMode) -> Result<CodegenReport, CodegenError> {
    let package_name = read_package_name(root)?;
    let project_config = config::load_config(root)?;
    let schemas_dir = root.join(SCHEMAS_DIR);
    for (index, workflow) in project_config.workflows.iter().enumerate() {
        for (field, path) in [
            ("input_schema", &workflow.input_schema_path),
            ("output_schema", &workflow.output_schema_path),
        ] {
            if path.parent() != Some(schemas_dir.as_path()) {
                return Err(CodegenError::SchemaOutsideSchemasDir {
                    field: format!("workflow[{index}].{field}"),
                    path: path.clone(),
                });
            }
        }
    }

    let artifacts = parse_project_schemas(root)?;

    let contents = emit::emit_module(&package_name, &artifacts);
    let module_relative = format!("src/{package_name}_io.gleam");
    let module_path = root.join("src").join(format!("{package_name}_io.gleam"));
    let written = match mode {
        CodegenMode::Write => {
            std::fs::write(&module_path, &contents).map_err(|source| CodegenError::Write {
                path: module_path.clone(),
                source,
            })?;
            true
        }
        CodegenMode::Check => {
            check_on_disk(&module_path, &contents)?;
            false
        }
    };

    Ok(CodegenReport {
        module_path,
        module_relative,
        schemas: artifacts
            .iter()
            .map(|artifact| artifact.file.display().to_string())
            .collect(),
        contents,
        written,
    })
}

/// Parses every `schemas/*.json` document under `root` into a deterministic,
/// filename-ordered artifact list, with names routed through one shared
/// [`NameRegistry`] so collisions across schemas fail loudly. Both
/// [`codegen_project`] and the activity generator parse from this single
/// source so their generated codecs cannot diverge.
///
/// # Errors
///
/// Returns a [`CodegenError`] for a missing/unreadable `schemas/` directory,
/// invalid JSON, an unsupported schema construct, or a generated-name
/// collision — each naming the offending file and JSON pointer.
pub(crate) fn parse_project_schemas(root: &Path) -> Result<Vec<SchemaArtifact>, CodegenError> {
    let schemas_dir = root.join(SCHEMAS_DIR);
    let file_names = list_schema_file_names(&schemas_dir)?;
    let mut registry = NameRegistry::default();
    let mut artifacts: Vec<SchemaArtifact> = Vec::with_capacity(file_names.len());
    for file_name in &file_names {
        artifacts.push(parse_one_schema(&schemas_dir, file_name, &mut registry)?);
    }
    Ok(artifacts)
}

/// Lists `*.json` file names directly under `schemas/`, sorted by byte
/// order. Non-JSON entries and subdirectories are outside the codegen
/// contract and are not generated from.
fn list_schema_file_names(schemas_dir: &Path) -> Result<Vec<String>, CodegenError> {
    let entries = match std::fs::read_dir(schemas_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(CodegenError::SchemasDirMissing {
                path: schemas_dir.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(CodegenError::SchemasDirRead {
                path: schemas_dir.to_path_buf(),
                source,
            });
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CodegenError::SchemasDirRead {
            path: schemas_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(CodegenError::SchemaFileName {
                path,
                reason: "file name is not valid UTF-8".to_owned(),
            });
        };
        names.push(name.to_owned());
    }
    if names.is_empty() {
        return Err(CodegenError::SchemasDirEmpty {
            path: schemas_dir.to_path_buf(),
        });
    }
    names.sort();
    Ok(names)
}

/// Reads, parses (order-preserving), and converts one schema file.
fn parse_one_schema(
    schemas_dir: &Path,
    file_name: &str,
    registry: &mut NameRegistry,
) -> Result<SchemaArtifact, CodegenError> {
    let path = schemas_dir.join(file_name);
    let relative = PathBuf::from(SCHEMAS_DIR).join(file_name);
    let Some(stem) = Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
    else {
        return Err(CodegenError::SchemaFileName {
            path: relative,
            reason: "file name has no stem".to_owned(),
        });
    };
    let bytes = std::fs::read(&path).map_err(|source| CodegenError::SchemaRead {
        path: path.clone(),
        source,
    })?;
    let document = json::parse_ordered(&bytes).map_err(|source| CodegenError::SchemaParse {
        path: path.clone(),
        source,
    })?;
    schema::parse_schema(&relative, stem, &document, registry)
}

pub(crate) fn check_on_disk(module_path: &Path, contents: &str) -> Result<(), CodegenError> {
    let on_disk = match std::fs::read(module_path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(CodegenError::CheckMissing {
                path: module_path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(CodegenError::CheckRead {
                path: module_path.to_path_buf(),
                source,
            });
        }
    };
    if on_disk != contents.as_bytes() {
        return Err(CodegenError::CheckDrift {
            path: module_path.to_path_buf(),
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GleamTomlName {
    name: String,
}

/// Reads the Gleam package name from `<root>/gleam.toml`; it prefixes the
/// generated module (`src/<name>_io.gleam`).
pub(crate) fn read_package_name(root: &Path) -> Result<String, CodegenError> {
    let path = root.join("gleam.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(CodegenError::Config(PackagingError::GleamTomlMissing {
                path,
            }));
        }
        Err(source) => {
            return Err(CodegenError::Config(PackagingError::GleamMetadataRead {
                path,
                source,
            }));
        }
    };
    let parsed: GleamTomlName = toml::from_str(&text).map_err(|source| {
        CodegenError::Config(PackagingError::GleamMetadataParse { path, source })
    })?;
    if !is_snake_identifier(&parsed.name) || is_reserved_word(&parsed.name) {
        return Err(CodegenError::ProjectName {
            name: parsed.name,
            reason: "must be a snake_case identifier and not a Gleam reserved word".to_owned(),
        });
    }
    Ok(parsed.name)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{CodegenMode, codegen_project, list_schema_file_names};
    use crate::PackagingError;
    use crate::codegen::error::CodegenError;
    use crate::project::fixture;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GLEAM_TOML: &str = "name = \"demo\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n";

    const WORKFLOW_TOML: &str = r#"[[workflow]]
entry_module = "demo"
entry_function = "run"
timeout_seconds = 30
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"
activities = []
"#;

    const INPUT_SCHEMA: &[u8] = br#"{
  "type": "object",
  "required": ["name"],
  "additionalProperties": false,
  "properties": {
    "name": { "type": "string" },
    "note": { "type": "string" }
  }
}"#;

    const OUTPUT_SCHEMA: &[u8] = br#"{ "type": "string" }"#;

    fn project(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        fixture::temp_project(
            label,
            &[
                ("gleam.toml", GLEAM_TOML.as_bytes()),
                ("workflow.toml", WORKFLOW_TOML.as_bytes()),
                ("schemas/input.json", INPUT_SCHEMA),
                ("schemas/output.json", OUTPUT_SCHEMA),
                ("src/demo.gleam", b"pub fn run() { Nil }"),
            ],
        )
    }

    #[test]
    fn write_mode_generates_the_module_with_header_and_report() -> TestResult {
        let root = project("codegen-write")?;
        let report = codegen_project(&root, CodegenMode::Write)?;

        assert!(report.written);
        assert_eq!(report.module_relative, "src/demo_io.gleam");
        assert_eq!(report.module_path, root.join("src/demo_io.gleam"));
        assert_eq!(
            report.schemas,
            vec![
                "schemas/input.json".to_owned(),
                "schemas/output.json".to_owned()
            ]
        );
        let on_disk = fs::read_to_string(&report.module_path)?;
        assert_eq!(on_disk, report.contents);
        assert!(on_disk.starts_with(
            "//// Generated by aion codegen — do not edit; regenerate from schemas/."
        ));
        assert!(on_disk.contains("pub type Input {"));
        assert!(on_disk.contains("pub fn output_decoder() -> decode.Decoder(String) {"));
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn generation_is_deterministic_across_runs() -> TestResult {
        let root = project("codegen-deterministic")?;
        let first = codegen_project(&root, CodegenMode::Write)?;
        let first_bytes = fs::read(&first.module_path)?;
        let second = codegen_project(&root, CodegenMode::Write)?;
        let second_bytes = fs::read(&second.module_path)?;

        assert_eq!(first.contents, second.contents);
        assert_eq!(
            first_bytes, second_bytes,
            "regeneration must be byte-identical"
        );
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn check_mode_passes_clean_and_fails_on_drift_naming_the_file() -> TestResult {
        let root = project("codegen-check")?;
        let written = codegen_project(&root, CodegenMode::Write)?;

        let checked = codegen_project(&root, CodegenMode::Check)?;
        assert!(!checked.written);
        assert_eq!(checked.contents, written.contents);

        let mut perturbed = fs::read_to_string(&written.module_path)?;
        perturbed.push_str("\n// hand edit\n");
        fs::write(&written.module_path, &perturbed)?;
        let result = codegen_project(&root, CodegenMode::Check);
        let Err(CodegenError::CheckDrift { path }) = result else {
            return Err(format!("expected CheckDrift, got {result:?}").into());
        };
        assert_eq!(path, written.module_path);
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn check_mode_fails_when_the_module_is_missing() -> TestResult {
        let root = project("codegen-check-missing")?;

        let result = codegen_project(&root, CodegenMode::Check);
        let Err(CodegenError::CheckMissing { path }) = result else {
            return Err(format!("expected CheckMissing, got {result:?}").into());
        };
        assert_eq!(path, root.join("src/demo_io.gleam"));
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn missing_referenced_schema_fails_through_descriptor_validation() -> TestResult {
        let root = fixture::temp_project(
            "codegen-missing-ref",
            &[
                ("gleam.toml", GLEAM_TOML.as_bytes()),
                ("workflow.toml", WORKFLOW_TOML.as_bytes()),
                ("schemas/output.json", OUTPUT_SCHEMA),
            ],
        )?;

        let result = codegen_project(&root, CodegenMode::Write);
        assert!(
            matches!(
                result,
                Err(CodegenError::Config(PackagingError::SchemaRead { ref path, .. }))
                    if *path == root.join("schemas/input.json")
            ),
            "missing referenced schema must fail: {result:?}"
        );
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn referenced_schema_outside_schemas_dir_is_rejected() -> TestResult {
        let descriptor = WORKFLOW_TOML.replace("schemas/input.json", "io/input.json");
        let root = fixture::temp_project(
            "codegen-outside",
            &[
                ("gleam.toml", GLEAM_TOML.as_bytes()),
                ("workflow.toml", descriptor.as_bytes()),
                ("io/input.json", INPUT_SCHEMA),
                ("schemas/output.json", OUTPUT_SCHEMA),
            ],
        )?;

        let result = codegen_project(&root, CodegenMode::Write);
        let Err(CodegenError::SchemaOutsideSchemasDir { field, path }) = result else {
            return Err(format!("expected SchemaOutsideSchemasDir, got {result:?}").into());
        };
        assert_eq!(field, "workflow[0].input_schema");
        assert_eq!(path, root.join("io/input.json"));
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn unsupported_construct_aborts_before_any_write() -> TestResult {
        let root = project("codegen-no-partial")?;
        fixture::write_file(
            &root,
            "schemas/zz_tagged.json",
            br#"{ "oneOf": [ { "type": "object", "properties": {} } ] }"#,
        )?;

        let result = codegen_project(&root, CodegenMode::Write);
        let Err(CodegenError::UnsupportedConstruct { file, pointer, .. }) = result else {
            return Err(format!("expected UnsupportedConstruct, got {result:?}").into());
        };
        assert_eq!(file, Path::new("schemas/zz_tagged.json"));
        assert_eq!(pointer, "/oneOf");
        assert!(
            !root.join("src/demo_io.gleam").exists(),
            "a failed run must not leave a partial module behind"
        );
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn non_json_entries_in_schemas_are_not_generated_from() -> TestResult {
        let root = project("codegen-non-json")?;
        fixture::write_file(&root, "schemas/README.md", b"docs, not a schema")?;
        fixture::write_file(
            &root,
            "schemas/nested/extra.json",
            br#"{ "type": "string" }"#,
        )?;

        let report = codegen_project(&root, CodegenMode::Write)?;
        assert_eq!(
            report.schemas,
            vec![
                "schemas/input.json".to_owned(),
                "schemas/output.json".to_owned()
            ]
        );
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn schemas_dir_listing_errors_are_typed() -> TestResult {
        let missing = std::env::temp_dir().join("aion-codegen-no-such-dir");
        let result = list_schema_file_names(&missing);
        assert!(matches!(
            result,
            Err(CodegenError::SchemasDirMissing { ref path }) if *path == missing
        ));

        let empty = fixture::temp_project("codegen-empty-schemas", &[("schemas/.keep", b"")])?;
        let result = list_schema_file_names(&empty.join("schemas"));
        assert!(matches!(result, Err(CodegenError::SchemasDirEmpty { .. })));
        fs::remove_dir_all(&empty)?;
        Ok(())
    }

    #[test]
    fn gleam_toml_problems_are_typed() -> TestResult {
        let root = fixture::temp_project(
            "codegen-no-gleam-toml",
            &[("workflow.toml", WORKFLOW_TOML.as_bytes())],
        )?;
        let result = codegen_project(&root, CodegenMode::Write);
        assert!(matches!(
            result,
            Err(CodegenError::Config(
                PackagingError::GleamTomlMissing { .. }
            ))
        ));
        fs::remove_dir_all(&root)?;

        let bad_name = fixture::temp_project(
            "codegen-bad-name",
            &[
                ("gleam.toml", b"name = \"Demo-App\"\n"),
                ("workflow.toml", WORKFLOW_TOML.as_bytes()),
                ("schemas/input.json", INPUT_SCHEMA),
                ("schemas/output.json", OUTPUT_SCHEMA),
            ],
        )?;
        let result = codegen_project(&bad_name, CodegenMode::Write);
        assert!(matches!(
            result,
            Err(CodegenError::ProjectName { ref name, .. }) if name == "Demo-App"
        ));
        fs::remove_dir_all(&bad_name)?;
        Ok(())
    }

    /// A schema that factors its shape through `$defs`/`$ref` is outside the v1
    /// subset and must fail loudly, naming the file and pointer. Built as a
    /// fixture: every stacked-dev example schema is in-subset since the
    /// brief-dev migration, so the example no longer carries an out-of-subset
    /// case to assert against.
    #[test]
    fn schema_outside_subset_hits_the_loud_error() -> TestResult {
        const FACTORED_WORKFLOW_TOML: &str = r#"[[workflow]]
entry_module = "demo"
entry_function = "run"
timeout_seconds = 30
input_schema = "schemas/factored.json"
output_schema = "schemas/output.json"
activities = []
"#;
        const FACTORED_SCHEMA: &[u8] = br##"{
  "type": "object",
  "properties": { "workspace": { "$ref": "#/$defs/workspace" } },
  "$defs": { "workspace": { "type": "object", "properties": {} } }
}"##;
        let root = fixture::temp_project(
            "codegen-outside-subset",
            &[
                ("gleam.toml", GLEAM_TOML.as_bytes()),
                ("workflow.toml", FACTORED_WORKFLOW_TOML.as_bytes()),
                ("schemas/factored.json", FACTORED_SCHEMA),
                ("schemas/output.json", OUTPUT_SCHEMA),
            ],
        )?;

        let result = codegen_project(&root, CodegenMode::Check);
        let Err(CodegenError::UnsupportedConstruct {
            file,
            pointer,
            construct,
        }) = result
        else {
            return Err(format!("expected UnsupportedConstruct, got {result:?}").into());
        };
        assert_eq!(file, Path::new("schemas/factored.json"));
        assert_eq!(pointer, "/$defs");
        assert!(construct.contains("unrecognised keyword `$defs`"));
        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
