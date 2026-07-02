//! The activity-plumbing generator (`aion generate`).
//!
//! Given a package's parsed activity declarations (extracted from its typed
//! Gleam `manifest()` by the CLI — this library never spawns a process) and
//! its boundary-type model (mapped from the exported package interface by
//! [`super::interface`]), this module resolves each declared value type to its
//! boundary type and emits the plumbing that used to be hand-mirrored across
//! five-to-seven files: the codecs module, the typed activity wrappers, the
//! per-tier worker handler stubs and registration, and the wire-compat golden.
//!
//! Generation is a pure, deterministic function of the declarations and the
//! model: declaration order and field order are preserved, names come from
//! pure helpers, and nothing reads the wall clock or iterates a `HashMap`
//! into output. The whole artifact set is rendered before any file is
//! touched, so a resolution error leaves the tree untouched, and a
//! delete-all-and-regenerate round-trip is byte-identical.
//! [`CodegenMode::Check`] re-renders and byte-compares instead of writing,
//! for CI drift gates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::activity_model::{ResolvedActivity, ResolvedType};
use super::codec_module;
use super::declaration::{ActivityDeclaration, Tier};
use super::error::CodegenError;
use super::model::{BoundaryType, GleamType};
use super::project::{CodegenMode, check_on_disk, read_package_name};
use super::test_scaffold::{self, WorkflowTestFacts};
use super::{activity_golden, activity_worker_python, activity_worker_rust, activity_wrappers};
use crate::structure::extract_workflow_facts;

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
/// module `src/<package>_codecs.gleam` from its boundary types.
///
/// The codecs are derived from the types module alone — not the activity
/// declarations — so this runs *before* the package's `manifest()` is
/// executed to extract the declarations: the author's activities module
/// references `codecs.<type>_codec()`, which must already compile for the
/// extraction build to succeed. [`generate_activities`] emits the remaining
/// plumbing (wrappers, worker, golden) once the declarations are known.
///
/// # Errors
///
/// Returns a [`CodegenError`] for an unreadable `gleam.toml`, a write
/// failure, or — in check mode — a missing or drifted codecs module.
pub fn generate_codecs(
    root: &Path,
    types: &[BoundaryType],
    mode: CodegenMode,
) -> Result<CodecReport, CodegenError> {
    let package_name = read_package_name(root)?;
    let relative = format!("src/{package_name}_codecs.gleam");
    let artifact = ActivityArtifact {
        path: root.join(&relative),
        relative: relative.clone(),
        contents: codec_module::emit_codecs_module(&package_name, types),
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
/// for the package at `root` from its `declarations` and boundary `types`.
///
/// # Errors
///
/// Returns a [`CodegenError`] for an unreadable `gleam.toml`, a declared
/// value type not present in the types module
/// ([`CodegenError::ActivityTypeMissing`]), a write failure, or — in check
/// mode — a missing or drifted generated file. No file is written if
/// rendering any artifact fails.
pub fn generate_activities(
    root: &Path,
    declarations: &[ActivityDeclaration],
    types: &[BoundaryType],
    mode: CodegenMode,
) -> Result<ActivityReport, CodegenError> {
    let package_name = read_package_name(root)?;
    let resolved = resolve(&package_name, declarations, types)?;
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

/// The result of generating (or checking) a workflow's `aion/testing` skeleton.
#[derive(Debug)]
pub struct TestScaffoldReport {
    /// The scaffold module path, relative to the project root.
    pub module_relative: String,
    /// The number of activities pre-mocked in a freshly-written scaffold.
    pub mocked_activities: usize,
    /// The number of clock advances scaffolded (one per durable timer).
    pub timer_advances: usize,
    /// Whether the module was written (`false` when it already existed or in
    /// check mode).
    pub written: bool,
}

/// Generates the `aion/testing` skeleton
/// `test/<entry_module>_scaffold_test.gleam` for the workflow whose typed
/// entry lives in `src/<entry_module>.gleam`.
///
/// Unlike the do-not-edit artifacts, the scaffold is written **once**: it is
/// a fill-in starting point the author owns after generation, so a write run
/// leaves an existing scaffold untouched, and [`CodegenMode::Check`] only
/// requires the file to exist (never byte-compares it against a fresh
/// render, which would flag every filled-in `todo` as drift).
///
/// # Errors
///
/// Returns a [`CodegenError`] for an unreadable `gleam.toml`, an unresolved
/// declared value type, an unreadable or facts-less entry-module source, a
/// write failure, or — in check mode — a missing scaffold.
pub fn generate_test_scaffold(
    root: &Path,
    entry_module: &str,
    declarations: &[ActivityDeclaration],
    types: &[BoundaryType],
    mode: CodegenMode,
) -> Result<TestScaffoldReport, CodegenError> {
    let package_name = read_package_name(root)?;
    let resolved = resolve(&package_name, declarations, types)?;

    let source_path = root.join("src").join(format!("{entry_module}.gleam"));
    let source =
        std::fs::read_to_string(&source_path).map_err(|source| CodegenError::EntrySourceRead {
            path: source_path.clone(),
            source,
        })?;
    let facts = extract_workflow_facts(&source).map_err(|error| CodegenError::ScaffoldFacts {
        path: source_path.clone(),
        reason: error.to_string(),
    })?;

    let relative = format!("test/{entry_module}_scaffold_test.gleam");
    let path = root.join(&relative);
    if mode == CodegenMode::Check {
        if !path.is_file() {
            return Err(CodegenError::CheckMissing { path });
        }
        return Ok(TestScaffoldReport {
            module_relative: relative,
            mocked_activities: resolved.len(),
            timer_advances: facts.timer_count,
            written: false,
        });
    }
    if path.is_file() {
        // The author owns a scaffold once it exists; never clobber their
        // filled-in test.
        return Ok(TestScaffoldReport {
            module_relative: relative,
            mocked_activities: resolved.len(),
            timer_advances: facts.timer_count,
            written: false,
        });
    }

    let test_facts = WorkflowTestFacts {
        entry_module,
        entry_function: &facts.typed_entry_function,
        timer_count: facts.timer_count,
    };
    let artifact = ActivityArtifact {
        path,
        relative: relative.clone(),
        contents: test_scaffold::emit_scaffold_module(&package_name, &test_facts, &resolved),
    };
    write_artifact(&artifact)?;

    Ok(TestScaffoldReport {
        module_relative: relative,
        mocked_activities: resolved.len(),
        timer_advances: facts.timer_count,
        written: true,
    })
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

/// Resolves each declaration's input and output value types to the boundary
/// types that generate their codecs, preserving declaration order.
fn resolve<'a>(
    package_name: &str,
    declarations: &'a [ActivityDeclaration],
    types: &'a [BoundaryType],
) -> Result<Vec<ResolvedActivity<'a>>, CodegenError> {
    let mut by_type: HashMap<&str, (&BoundaryType, &str)> = HashMap::with_capacity(types.len());
    for boundary in types {
        if let GleamType::Named {
            type_name,
            fn_prefix,
        } = &boundary.root
        {
            by_type.insert(type_name.as_str(), (boundary, fn_prefix.as_str()));
        }
    }

    let mut resolved = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let input = resolve_type(
            package_name,
            declaration,
            "input",
            &declaration.input_type,
            &by_type,
        )?;
        let output = resolve_type(
            package_name,
            declaration,
            "output",
            &declaration.output_type,
            &by_type,
        )?;
        resolved.push(ResolvedActivity {
            declaration,
            input,
            output,
        });
    }
    Ok(resolved)
}

/// Resolves one declared value type name to its boundary type.
fn resolve_type<'a>(
    package_name: &str,
    declaration: &ActivityDeclaration,
    role: &'static str,
    type_name: &str,
    by_type: &HashMap<&str, (&'a BoundaryType, &str)>,
) -> Result<ResolvedType<'a>, CodegenError> {
    let (boundary, fn_prefix) =
        by_type
            .get(type_name)
            .ok_or_else(|| CodegenError::ActivityTypeMissing {
                activity: declaration.name.clone(),
                role,
                type_name: type_name.to_owned(),
                module: format!("{package_name}_io"),
            })?;
    Ok(ResolvedType {
        gleam_type: type_name.to_owned(),
        fn_prefix: (*fn_prefix).to_owned(),
        boundary,
    })
}

/// Renders every generated activity artifact in a deterministic order: the
/// typed wrappers module first, then a worker plumbing module and (for remote
/// tiers) a wire-compat golden per non-empty tier. The codecs module is owned
/// by [`generate_codecs`] (it is types-driven and must precede declaration
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
    // shape against a literal derived from the type, worker-language-agnostic.
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{generate_activities, generate_codecs};
    use crate::codegen::declaration::{ActivityDeclaration, Tier};
    use crate::codegen::error::CodegenError;
    use crate::codegen::model::{BoundaryType, Field, GleamType, RecordDef, TypeDef};
    use crate::codegen::project::CodegenMode;
    use crate::project::fixture;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GLEAM_TOML: &str = "name = \"demo\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n";

    fn boundary(type_name: &str, fields: Vec<(&str, GleamType, bool)>) -> BoundaryType {
        let stem = crate::codegen::names::pascal_to_snake(type_name);
        BoundaryType {
            file: PathBuf::from(format!("schemas/{stem}.json")),
            stem: stem.clone(),
            root: GleamType::Named {
                type_name: type_name.to_owned(),
                fn_prefix: stem.clone(),
            },
            defs: vec![TypeDef::Record(RecordDef {
                type_name: type_name.to_owned(),
                fn_prefix: stem,
                fields: fields
                    .into_iter()
                    .map(|(wire, ty, required)| Field {
                        wire: wire.to_owned(),
                        ty,
                        required,
                    })
                    .collect(),
            })],
        }
    }

    fn model() -> Vec<BoundaryType> {
        vec![
            boundary(
                "Order",
                vec![
                    ("order_id", GleamType::String, true),
                    ("amount", GleamType::Int, true),
                ],
            ),
            boundary("Receipt", vec![("payment_id", GleamType::String, true)]),
        ]
    }

    fn project(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        fixture::temp_project(label, &[("gleam.toml", GLEAM_TOML.as_bytes())])
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
        let types = model();
        let declarations = [declaration("Order", "Receipt")];

        let codecs = generate_codecs(&root, &types, CodegenMode::Write)?;
        assert!(codecs.written);
        assert!(root.join("src/demo_codecs.gleam").is_file());

        let report = generate_activities(&root, &declarations, &types, CodegenMode::Write)?;
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
        generate_codecs(&root, &types, CodegenMode::Check)?;
        generate_activities(&root, &declarations, &types, CodegenMode::Check)?;

        // A hand-edit to a generated file is caught.
        let wrappers = root.join("src/demo_activity_wrappers.gleam");
        let mut tampered = fs::read_to_string(&wrappers)?;
        tampered.push_str("\n// hand edit\n");
        fs::write(&wrappers, &tampered)?;
        let result = generate_activities(&root, &declarations, &types, CodegenMode::Check);
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
        let types = model();
        let declarations = [ActivityDeclaration {
            name: "charge".to_owned(),
            tier: Tier::InVm,
            input_type: "Order".to_owned(),
            output_type: "Receipt".to_owned(),
        }];

        let report = generate_activities(&root, &declarations, &types, CodegenMode::Write)?;
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
    fn declared_type_without_a_boundary_type_errors() -> TestResult {
        let root = project("activity-missing")?;
        let types = model();
        let declarations = [declaration("Order", "NoSuchType")];

        let result = generate_activities(&root, &declarations, &types, CodegenMode::Write);
        let Err(CodegenError::ActivityTypeMissing {
            activity,
            role,
            type_name,
            module,
        }) = result
        else {
            fs::remove_dir_all(&root)?;
            return Err(format!("expected ActivityTypeMissing, got {result:?}").into());
        };
        assert_eq!(activity, "charge");
        assert_eq!(role, "output");
        assert_eq!(type_name, "NoSuchType");
        assert_eq!(module, "demo_io");

        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
