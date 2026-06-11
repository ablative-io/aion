//! `package_project` pipeline: config → discovery → build → verify-after-write.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{
    config::{self, WorkflowConfig},
    discover,
    error::PackagingError,
};
use crate::{
    BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion, Package,
    PackageBuilder, WorkflowVersion,
};

/// Options for packaging an already-built Gleam workflow project.
///
/// Construct via [`Default`] and assign fields, so call sites keep compiling
/// when options are added.
#[derive(Clone, Debug, Default)]
pub struct PackageOptions {
    /// Overrides the single workflow's output path, resolved against the
    /// project root when relative. Packaging fails with
    /// [`PackagingError::OutputOverrideAmbiguous`] when the project declares
    /// more than one workflow.
    pub output_override: Option<PathBuf>,
}

/// Result of packaging every workflow a project declares.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectReport {
    /// One built package per `[[workflow]]` entry, in declaration order.
    pub packages: Vec<PackagedWorkflow>,
    /// Modules excluded by the SDK test filter or the dependency-closure filter.
    pub excluded: Vec<ExcludedModule>,
}

/// One workflow archive written and verified by [`package_project`].
#[derive(Clone, Debug, PartialEq)]
pub struct PackagedWorkflow {
    /// Workflow type, identical to the manifest entry module.
    pub workflow_type: String,
    /// Absolute path of the written `.aion` archive.
    pub output_path: PathBuf,
    /// The archive re-loaded from disk after writing, proving integrity.
    pub package: Package,
    /// Canonical version record of the verified package.
    pub version: WorkflowVersion,
}

/// A compiled module excluded from packaging, with provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExcludedModule {
    /// Logical module name that was excluded.
    pub module: String,
    /// Gleam package whose ebin provided the module.
    pub package: String,
    /// Why the module was excluded.
    pub reason: ExcludedReason,
}

/// Reason a discovered compiled module was excluded from packaging.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcludedReason {
    /// SDK test machinery from the `aion_flow` package's ebin.
    SdkTestOnly,
    /// Module of a package outside the production dependency closure.
    DevDependency,
}

/// Packages every workflow declared by `<root>/workflow.toml`.
///
/// The project must already be built (`gleam build`); this function never
/// spawns processes. The pipeline parses and validates the descriptor,
/// discovers the production-closure compiled modules once, then writes one
/// deterministic `.aion` archive per `[[workflow]]` entry. Every written
/// archive is re-loaded through [`Package::load_from_path`] before this
/// function returns, so the full read-path validation (integrity hash, format
/// version, entry module) gates success.
///
/// All archives from one project share a single content hash (it covers beams
/// only), while deployed entry names remain distinct per entry module. First
/// party sources ship by default and never affect the hash.
///
/// Pure with respect to the environment: reads only under `root` (which is
/// made absolute against the current directory once, up front), writes only
/// the declared outputs, reads no environment variables, never prints, and
/// blocks on synchronous filesystem I/O — async callers should wrap it in a
/// blocking task.
///
/// # Errors
///
/// Returns [`PackagingError`] variants for missing or invalid `workflow.toml`
/// descriptors, unreadable or non-JSON schema files, unbuilt projects, broken
/// Gleam metadata, unresolved dependencies, duplicate or unreadable compiled
/// modules, missing entry modules, output conflicts, ambiguous output
/// overrides, and archive write or verify-after-write failures.
pub fn package_project(
    root: &Path,
    options: &PackageOptions,
) -> Result<ProjectReport, PackagingError> {
    let root = std::path::absolute(root).map_err(|source| PackagingError::ConfigRead {
        path: root.to_path_buf(),
        source,
    })?;

    let mut config = config::load_config(&root)?;
    apply_output_override(&root, options, &mut config.workflows)?;

    let discovered = discover::discover_modules(&root)?;
    let beams = BeamSet::new(discovered.modules)?;
    let source = if config.include_source {
        discover::discover_sources(&root)?
    } else {
        BTreeMap::new()
    };

    let mut packages = Vec::with_capacity(config.workflows.len());
    for workflow in &config.workflows {
        if beams.get(&workflow.entry_module).is_none() {
            return Err(PackagingError::EntryModuleNotFound {
                module: workflow.entry_module.clone(),
                searched: discovered.searched.clone(),
            });
        }
        packages.push(build_workflow_package(workflow, &beams, &source)?);
    }

    Ok(ProjectReport {
        packages,
        excluded: discovered.excluded,
    })
}

fn apply_output_override(
    root: &Path,
    options: &PackageOptions,
    workflows: &mut [WorkflowConfig],
) -> Result<(), PackagingError> {
    let Some(output_override) = &options.output_override else {
        return Ok(());
    };
    match workflows {
        [workflow] => {
            workflow.output_path = root.join(output_override);
            Ok(())
        }
        _ => Err(PackagingError::OutputOverrideAmbiguous {
            count: workflows.len(),
        }),
    }
}

fn build_workflow_package(
    workflow: &WorkflowConfig,
    beams: &BeamSet,
    source: &BTreeMap<String, Vec<u8>>,
) -> Result<PackagedWorkflow, PackagingError> {
    let manifest = Manifest {
        entry_module: workflow.entry_module.clone(),
        entry_function: workflow.entry_function.clone(),
        input_schema: workflow.input_schema.clone(),
        output_schema: workflow.output_schema.clone(),
        timeout: workflow.timeout,
        activities: workflow
            .activities
            .iter()
            .map(|activity_type| DeclaredActivity {
                activity_type: activity_type.clone(),
            })
            .collect(),
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
    };

    PackageBuilder::with_source(manifest, beams.clone(), source.clone())
        .write_to_path(&workflow.output_path)?;
    let package = Package::load_from_path(&workflow.output_path)?;

    Ok(PackagedWorkflow {
        workflow_type: workflow.entry_module.clone(),
        output_path: workflow.output_path.clone(),
        version: package.version_record(),
        package,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::Duration};

    use serde_json::json;

    use super::{ExcludedModule, ExcludedReason, PackageOptions, package_project};
    use crate::{PackageError, project::error::PackagingError, project::fixture};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const TWO_WORKFLOW_TOML: &str = r#"[[workflow]]
entry_module = "demo"
entry_function = "run"
timeout_seconds = 30
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"
activities = ["greet"]

[[workflow]]
entry_module = "demo@nested"
entry_function = "start"
timeout_seconds = 60
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"
activities = []
"#;

    #[test]
    fn packaged_workflow_round_trips_manifest_and_hash() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-happy")?;
        let report = package_project(&root, &PackageOptions::default());
        let reloaded = report
            .as_ref()
            .ok()
            .map(|report| report.packages[0].output_path.clone())
            .map(crate::Package::load_from_path);
        fs::remove_dir_all(&root)?;
        let report = report?;

        assert_eq!(report.packages.len(), 1);
        let packaged = &report.packages[0];
        assert_eq!(packaged.workflow_type, "demo");
        assert!(packaged.output_path.is_absolute());
        assert_eq!(
            packaged
                .output_path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("demo.aion")
        );
        let manifest = packaged.package.manifest();
        assert_eq!(manifest.entry_module, "demo");
        assert_eq!(manifest.entry_function, "run");
        assert_eq!(manifest.timeout, Duration::from_secs(30));
        assert_eq!(manifest.input_schema, json!({ "type": "object" }));
        assert_eq!(manifest.output_schema, json!(true));
        assert_eq!(manifest.activities.len(), 1);
        assert_eq!(manifest.activities[0].activity_type, "greet");
        assert_eq!(
            manifest.version.as_str(),
            packaged.package.content_hash().to_string()
        );
        assert_eq!(packaged.version, packaged.package.version_record());
        let reloaded = reloaded.ok_or("report failed")??;
        assert_eq!(&reloaded, &packaged.package);
        Ok(())
    }

    #[test]
    fn exclusions_and_sources_are_reported_and_shipped() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-exclusions")?;
        let report = package_project(&root, &PackageOptions::default());
        fs::remove_dir_all(&root)?;
        let report = report?;

        let expected_exclusions = vec![
            ExcludedModule {
                module: "dev_only".to_owned(),
                package: "dev_only".to_owned(),
                reason: ExcludedReason::DevDependency,
            },
            ExcludedModule {
                module: "aion@testing".to_owned(),
                package: "aion_flow".to_owned(),
                reason: ExcludedReason::SdkTestOnly,
            },
            ExcludedModule {
                module: "aion@testing@mock".to_owned(),
                package: "aion_flow".to_owned(),
                reason: ExcludedReason::SdkTestOnly,
            },
            ExcludedModule {
                module: "aion_flow_ffi".to_owned(),
                package: "aion_flow".to_owned(),
                reason: ExcludedReason::SdkTestOnly,
            },
        ];
        assert_eq!(report.excluded, expected_exclusions);

        let package = &report.packages[0].package;
        let source_names: Vec<&str> = package.source().keys().map(String::as_str).collect();
        assert_eq!(source_names, vec!["demo", "demo/nested"]);
        let beam_names: Vec<&str> = package
            .beams()
            .iter()
            .map(crate::BeamModule::name)
            .collect();
        assert_eq!(
            beam_names,
            vec!["aion_flow", "demo", "demo@nested", "dep_a", "dep_b"]
        );
        Ok(())
    }

    #[test]
    fn missing_entry_module_returns_entry_module_not_found() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-ghost-entry")?;
        let descriptor = fixture::DEMO_WORKFLOW_TOML.replace("\"demo\"", "\"ghost\"");
        fixture::write_file(&root, "workflow.toml", descriptor.as_bytes())?;
        let result = package_project(&root, &PackageOptions::default());
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::EntryModuleNotFound { module, searched })
                if module == "ghost" && searched.ends_with("build/dev/erlang")
        ));
        Ok(())
    }

    #[test]
    fn explicit_output_field_is_respected() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-explicit-output")?;
        let descriptor = format!(
            "{}output = \"custom-name.aion\"\n",
            fixture::DEMO_WORKFLOW_TOML
        );
        fixture::write_file(&root, "workflow.toml", descriptor.as_bytes())?;
        let report = package_project(&root, &PackageOptions::default());
        let written = root.join("custom-name.aion").is_file();
        fs::remove_dir_all(&root)?;
        let report = report?;

        assert!(written);
        assert_eq!(
            report.packages[0]
                .output_path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("custom-name.aion")
        );
        Ok(())
    }

    #[test]
    fn output_override_applies_to_single_workflow_project() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-override")?;
        let options = PackageOptions {
            output_override: Some(PathBuf::from("override.aion")),
        };
        let report = package_project(&root, &options);
        let written = root.join("override.aion").is_file();
        let derived_absent = !root.join("demo.aion").exists();
        fs::remove_dir_all(&root)?;
        let report = report?;

        assert!(written);
        assert!(derived_absent);
        assert_eq!(
            report.packages[0]
                .output_path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("override.aion")
        );
        Ok(())
    }

    #[test]
    fn output_override_with_multiple_workflows_is_ambiguous() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-override-multi")?;
        fixture::write_file(&root, "workflow.toml", TWO_WORKFLOW_TOML.as_bytes())?;
        let options = PackageOptions {
            output_override: Some(PathBuf::from("override.aion")),
        };
        let result = package_project(&root, &options);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::OutputOverrideAmbiguous { count: 2 })
        ));
        Ok(())
    }

    #[test]
    fn multi_workflow_project_shares_hash_with_distinct_deployed_entries() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-multi")?;
        fixture::write_file(&root, "workflow.toml", TWO_WORKFLOW_TOML.as_bytes())?;
        let report = package_project(&root, &PackageOptions::default());
        fs::remove_dir_all(&root)?;
        let report = report?;

        assert_eq!(report.packages.len(), 2);
        let first = &report.packages[0];
        let second = &report.packages[1];
        assert_eq!(first.workflow_type, "demo");
        assert_eq!(second.workflow_type, "demo@nested");
        assert_eq!(first.package.content_hash(), second.package.content_hash());
        assert_ne!(
            first.package.deployed_entry_module(),
            second.package.deployed_entry_module()
        );
        assert_ne!(first.output_path, second.output_path);
        Ok(())
    }

    #[test]
    fn user_module_with_reserved_name_fails_typed() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-reserved")?;
        fixture::write_file(
            &root,
            "build/dev/erlang/demo/ebin/aion_flow_ffi.beam",
            b"user-owned-bytes",
        )?;
        let result = package_project(&root, &PackageOptions::default());
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::Package(PackageError::ReservedModuleName { module }))
                if module == "aion_flow_ffi"
        ));
        Ok(())
    }

    #[test]
    fn repackaging_produces_identical_archive_bytes() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-det-1")?;
        let first_report = package_project(&root, &PackageOptions::default());
        let first_bytes = fs::read(root.join("demo.aion"));
        let second_report = package_project(&root, &PackageOptions::default());
        let second_bytes = fs::read(root.join("demo.aion"));
        fs::remove_dir_all(&root)?;
        first_report?;
        second_report?;

        let first_bytes = first_bytes?;
        assert!(!first_bytes.is_empty());
        assert_eq!(first_bytes, second_bytes?);
        Ok(())
    }

    #[test]
    fn source_inclusion_changes_bytes_but_never_the_version() -> TestResult {
        let root = fixture::synthetic_built_project("assemble-det-3")?;
        let with_source = package_project(&root, &PackageOptions::default());
        let with_source_bytes = fs::read(root.join("demo.aion"));
        let descriptor = format!(
            "[package]\ninclude_source = false\n\n{}",
            fixture::DEMO_WORKFLOW_TOML
        );
        fixture::write_file(&root, "workflow.toml", descriptor.as_bytes())?;
        let without_source = package_project(&root, &PackageOptions::default());
        let without_source_bytes = fs::read(root.join("demo.aion"));
        fs::remove_dir_all(&root)?;
        let with_source = with_source?;
        let without_source = without_source?;

        assert!(!with_source.packages[0].package.source().is_empty());
        assert!(without_source.packages[0].package.source().is_empty());
        assert_ne!(with_source_bytes?, without_source_bytes?);
        assert_eq!(
            with_source.packages[0].package.content_hash(),
            without_source.packages[0].package.content_hash()
        );
        assert_eq!(
            with_source.packages[0].package.manifest().version,
            without_source.packages[0].package.manifest().version
        );
        Ok(())
    }
}
