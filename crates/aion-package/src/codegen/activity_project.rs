//! The activity-plumbing generator (`aion generate`).
//!
//! Given a package's parsed activity declarations (extracted from its typed
//! Gleam `manifest()` by the CLI — this library never spawns a process) and its
//! `schemas/*.json`, this module resolves each declared value type to its
//! schema artifact and emits the plumbing that today is hand-mirrored across
//! five-to-seven files: the codec module, the typed activity wrappers, the
//! per-tier worker handler stubs and registration, and the wire-compat golden.
//!
//! Generation is a pure, deterministic function of the declarations and
//! schemas: declaration order and schema property order are preserved, names
//! come from pure helpers, and nothing reads the wall clock or iterates a
//! `HashMap` into output. The whole artifact set is rendered before any file is
//! touched, so a parse or resolution error leaves the tree untouched, and a
//! delete-all-and-regenerate round-trip is byte-identical. [`CodegenMode::Check`]
//! re-renders and byte-compares instead of writing, for CI drift gates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::activity_model::{ResolvedActivity, ResolvedType};
use super::declaration::{ActivityDeclaration, Tier};
use super::error::CodegenError;
use super::project::{CodegenMode, check_on_disk, parse_project_schemas, read_package_name};
use super::schema::{GleamType, SchemaArtifact};
use super::{activity_golden, activity_worker_python, activity_worker_rust, activity_wrappers};

/// One generated file: its path and fully-rendered contents.
#[derive(Debug)]
pub struct ActivityArtifact {
    /// Absolute path the file is written to or checked against.
    pub path: PathBuf,
    /// Project-root-relative path, for the CLI's JSON report.
    pub relative: String,
    /// The fully-rendered file contents, built before any write.
    pub contents: String,
}

/// The result of generating (or checking) a package's activity plumbing.
#[derive(Debug)]
pub struct ActivityReport {
    /// Every generated file, in deterministic order.
    pub artifacts: Vec<ActivityArtifact>,
    /// Whether the files were written (`Write`) or only checked (`Check`).
    pub written: bool,
}

/// The result of generating (or checking) a package's codecs module.
#[derive(Debug)]
pub struct CodecReport {
    /// The generated codecs module path, relative to the project root.
    pub module_relative: String,
    /// Whether the module was written (`Write`) or only checked (`Check`).
    pub written: bool,
}

/// Generates (or, in [`CodegenMode::Check`], verifies) the package's codecs
/// module `src/<package>_codecs.gleam` from its `schemas/*.json`.
///
/// The codecs are derived from the schemas alone — not the activity
/// declarations — so this can run *before* the package's `manifest()` is
/// executed to extract the declarations: the author's activities module
/// references `codecs.<type>_codec()`, which must already compile for the
/// extraction build to succeed. [`generate_activities`] emits the remaining
/// plumbing (wrappers, worker, golden) once the declarations are known.
///
/// # Errors
///
/// Returns a [`CodegenError`] for an unreadable `gleam.toml`/`schemas/`, an
/// unsupported schema construct, a write failure, or — in check mode — a
/// missing or drifted codecs module.
pub fn generate_codecs(root: &Path, mode: CodegenMode) -> Result<CodecReport, CodegenError> {
    let package_name = read_package_name(root)?;
    let schemas = parse_project_schemas(root)?;
    let relative = format!("src/{package_name}_codecs.gleam");
    let artifact = ActivityArtifact {
        path: root.join(&relative),
        relative: relative.clone(),
        contents: activity_wrappers::emit_codecs_module(&package_name, &schemas),
    };
    let written = match mode {
        CodegenMode::Write => {
            write_artifact(&artifact)?;
            true
        }
        CodegenMode::Check => {
            check_on_disk(&artifact.path, &artifact.contents)?;
            false
        }
    };
    Ok(CodecReport {
        module_relative: relative,
        written,
    })
}

/// Generates (or, in [`CodegenMode::Check`], verifies) the activity plumbing
/// for the package at `root` from its `declarations` and `schemas/*.json`.
///
/// # Errors
///
/// Returns a [`CodegenError`] for an unreadable `gleam.toml`/`schemas/`, an
/// unsupported schema construct, a declared value type with no matching schema
/// ([`CodegenError::ActivitySchemaMissing`]), a write failure, or — in check
/// mode — a missing or drifted generated file. No file is written if rendering
/// any artifact fails.
pub fn generate_activities(
    root: &Path,
    declarations: &[ActivityDeclaration],
    mode: CodegenMode,
) -> Result<ActivityReport, CodegenError> {
    let package_name = read_package_name(root)?;
    let schemas = parse_project_schemas(root)?;
    let resolved = resolve(declarations, &schemas)?;
    let artifacts = build_artifacts(root, &package_name, &resolved)?;

    let written = match mode {
        CodegenMode::Write => {
            for artifact in &artifacts {
                write_artifact(artifact)?;
            }
            true
        }
        CodegenMode::Check => {
            for artifact in &artifacts {
                check_on_disk(&artifact.path, &artifact.contents)?;
            }
            false
        }
    };

    Ok(ActivityReport { artifacts, written })
}

/// Writes one artifact, creating any missing parent directories first.
fn write_artifact(artifact: &ActivityArtifact) -> Result<(), CodegenError> {
    if let Some(parent) = artifact.path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CodegenError::Write {
            path: artifact.path.clone(),
            source,
        })?;
    }
    std::fs::write(&artifact.path, &artifact.contents).map_err(|source| CodegenError::Write {
        path: artifact.path.clone(),
        source,
    })
}

/// Resolves each declaration's input and output value types to the schema
/// artifacts that generate them, preserving declaration order.
fn resolve<'a>(
    declarations: &'a [ActivityDeclaration],
    schemas: &'a [SchemaArtifact],
) -> Result<Vec<ResolvedActivity<'a>>, CodegenError> {
    // Map each generated Gleam type name to its artifact and function prefix.
    // Only object/enum schemas (named roots) back an activity value type.
    let mut by_type: HashMap<&str, (&SchemaArtifact, &str)> = HashMap::with_capacity(schemas.len());
    for artifact in schemas {
        if let GleamType::Named {
            type_name,
            fn_prefix,
        } = &artifact.root
        {
            by_type.insert(type_name.as_str(), (artifact, fn_prefix.as_str()));
        }
    }

    let mut resolved = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let input = resolve_type(declaration, "input", &declaration.input_type, &by_type)?;
        let output = resolve_type(declaration, "output", &declaration.output_type, &by_type)?;
        resolved.push(ResolvedActivity {
            declaration,
            input,
            output,
        });
    }
    Ok(resolved)
}

/// Resolves one declared value type name to its schema artifact.
fn resolve_type<'a>(
    declaration: &ActivityDeclaration,
    role: &'static str,
    type_name: &str,
    by_type: &HashMap<&str, (&'a SchemaArtifact, &str)>,
) -> Result<ResolvedType<'a>, CodegenError> {
    let (artifact, fn_prefix) =
        by_type
            .get(type_name)
            .ok_or_else(|| CodegenError::ActivitySchemaMissing {
                activity: declaration.name.clone(),
                role,
                type_name: type_name.to_owned(),
                path: PathBuf::from(format!("schemas/{}.json", to_snake(type_name))),
            })?;
    Ok(ResolvedType {
        gleam_type: type_name.to_owned(),
        fn_prefix: (*fn_prefix).to_owned(),
        artifact,
    })
}

/// Renders every generated activity artifact in a deterministic order: the
/// typed wrappers module first, then a worker plumbing module and (for remote
/// tiers) a wire-compat golden per non-empty tier. The codecs module is owned
/// by [`generate_codecs`] (it is schema-driven and must precede declaration
/// extraction), so it is not re-emitted here.
fn build_artifacts(
    root: &Path,
    package_name: &str,
    resolved: &[ResolvedActivity],
) -> Result<Vec<ActivityArtifact>, CodegenError> {
    let src = root.join("src");
    let mut artifacts = Vec::new();

    artifacts.push(gleam_module(
        &src,
        package_name,
        "activity_wrappers",
        activity_wrappers::emit_wrappers_module(package_name, resolved),
    ));

    // In-VM activities execute as the author's Gleam body referenced by the
    // generated wrapper; they have no separate worker artifact and never cross
    // the wire, so they get neither a worker stub nor a golden.
    let python = of_tier(resolved, Tier::RemotePython);
    let rust = of_tier(resolved, Tier::RemoteRust);

    if !python.is_empty() {
        artifacts.push(file(
            root,
            "worker/worker.py".to_owned(),
            activity_worker_python::emit(package_name, &python),
        ));
    }
    if !rust.is_empty() {
        artifacts.push(file(
            root,
            "worker/src/main.rs".to_owned(),
            activity_worker_rust::emit(package_name, &rust),
        ));
    }

    // One wire-compat golden covering every remote activity, in declaration
    // order: a Gleam SDK-side test that pins each value type's encoded wire
    // shape against a literal derived from the schema, worker-language-agnostic.
    let remote: Vec<&ResolvedActivity> = resolved
        .iter()
        .filter(|a| a.declaration.tier.is_remote())
        .collect();
    if !remote.is_empty() {
        artifacts.push(file(
            root,
            format!("test/{package_name}_wire_compat_test.gleam"),
            activity_golden::emit(package_name, &remote)?,
        ));
    }

    Ok(artifacts)
}

/// Collects references to the resolved activities of one tier, in declaration
/// order.
fn of_tier<'a, 'b>(
    resolved: &'b [ResolvedActivity<'a>],
    tier: Tier,
) -> Vec<&'b ResolvedActivity<'a>> {
    resolved
        .iter()
        .filter(|a| a.declaration.tier == tier)
        .collect()
}

/// Builds an artifact for a generated Gleam module `src/<pkg>_<suffix>.gleam`.
/// `src` is the package's absolute `src/` directory.
fn gleam_module(
    src: &Path,
    package_name: &str,
    suffix: &str,
    contents: String,
) -> ActivityArtifact {
    let file_name = format!("{package_name}_{suffix}.gleam");
    let relative = format!("src/{file_name}");
    ActivityArtifact {
        path: src.join(file_name),
        relative,
        contents,
    }
}

/// Builds an artifact for a generated file at a project-root-relative path.
fn file(root: &Path, relative: String, contents: String) -> ActivityArtifact {
    ActivityArtifact {
        path: root.join(&relative),
        relative,
        contents,
    }
}

/// Converts a Gleam type name to the snake-case schema stem it derives from,
/// for the "schema missing" error hint (`OrderInput` → `order_input`).
fn to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.char_indices() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{generate_activities, generate_codecs};
    use crate::codegen::declaration::{ActivityDeclaration, Tier};
    use crate::codegen::error::CodegenError;
    use crate::codegen::project::CodegenMode;
    use crate::project::fixture;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GLEAM_TOML: &str = "name = \"demo\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n";
    const ORDER_SCHEMA: &[u8] = br#"{
        "type": "object",
        "required": ["order_id", "amount"],
        "additionalProperties": false,
        "properties": {
            "order_id": { "type": "string" },
            "amount": { "type": "integer" }
        }
    }"#;
    const RECEIPT_SCHEMA: &[u8] = br#"{
        "type": "object",
        "required": ["payment_id"],
        "additionalProperties": false,
        "properties": { "payment_id": { "type": "string" } }
    }"#;

    fn project(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        fixture::temp_project(
            label,
            &[
                ("gleam.toml", GLEAM_TOML.as_bytes()),
                ("schemas/order.json", ORDER_SCHEMA),
                ("schemas/receipt.json", RECEIPT_SCHEMA),
            ],
        )
    }

    fn declaration(input: &str, output: &str) -> ActivityDeclaration {
        ActivityDeclaration {
            name: "charge".to_owned(),
            tier: Tier::RemotePython,
            input_type: input.to_owned(),
            output_type: output.to_owned(),
        }
    }

    #[test]
    fn write_then_check_round_trips_and_detects_drift() -> TestResult {
        let root = project("activity-write")?;
        let declarations = [declaration("Order", "Receipt")];

        let codecs = generate_codecs(&root, CodegenMode::Write)?;
        assert!(codecs.written);
        assert!(root.join("src/demo_codecs.gleam").is_file());

        let report = generate_activities(&root, &declarations, CodegenMode::Write)?;
        assert!(report.written);
        let relatives: Vec<&str> = report
            .artifacts
            .iter()
            .map(|artifact| artifact.relative.as_str())
            .collect();
        assert_eq!(
            relatives,
            vec![
                "src/demo_activity_wrappers.gleam",
                "worker/worker.py",
                "test/demo_wire_compat_test.gleam",
            ]
        );
        for artifact in &report.artifacts {
            assert!(artifact.path.is_file(), "{} not written", artifact.relative);
        }

        // A clean tree passes --check.
        generate_codecs(&root, CodegenMode::Check)?;
        generate_activities(&root, &declarations, CodegenMode::Check)?;

        // A hand-edit to a generated file is caught.
        let wrappers = root.join("src/demo_activity_wrappers.gleam");
        let mut tampered = fs::read_to_string(&wrappers)?;
        tampered.push_str("\n// hand edit\n");
        fs::write(&wrappers, &tampered)?;
        let result = generate_activities(&root, &declarations, CodegenMode::Check);
        let Err(CodegenError::CheckDrift { path }) = result else {
            fs::remove_dir_all(&root)?;
            return Err(format!("expected CheckDrift, got {result:?}").into());
        };
        assert_eq!(path, wrappers);

        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn in_vm_tier_emits_neither_worker_nor_golden() -> TestResult {
        let root = project("activity-invm")?;
        let declarations = [ActivityDeclaration {
            name: "charge".to_owned(),
            tier: Tier::InVm,
            input_type: "Order".to_owned(),
            output_type: "Receipt".to_owned(),
        }];

        let report = generate_activities(&root, &declarations, CodegenMode::Write)?;
        let relatives: Vec<&str> = report
            .artifacts
            .iter()
            .map(|artifact| artifact.relative.as_str())
            .collect();
        // Only the typed wrappers; an in-VM activity never crosses the wire.
        assert_eq!(relatives, vec!["src/demo_activity_wrappers.gleam"]);
        assert!(!root.join("worker/worker.py").exists());
        assert!(!root.join("test/demo_wire_compat_test.gleam").exists());

        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn declared_type_without_a_schema_errors() -> TestResult {
        let root = project("activity-missing")?;
        let declarations = [declaration("Order", "NoSuchType")];

        let result = generate_activities(&root, &declarations, CodegenMode::Write);
        let Err(CodegenError::ActivitySchemaMissing {
            activity,
            role,
            type_name,
            ..
        }) = result
        else {
            fs::remove_dir_all(&root)?;
            return Err(format!("expected ActivitySchemaMissing, got {result:?}").into());
        };
        assert_eq!(activity, "charge");
        assert_eq!(role, "output");
        assert_eq!(type_name, "NoSuchType");

        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
