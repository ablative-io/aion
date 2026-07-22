//! The dual-backend package builder.
//!
//! Reference backend: every lowering fixture is emitted to Gleam, dropped into
//! ONE throwaway project (its SDK closure compiled once), `gleam build` is run
//! ONCE, and `aion_package::package_project` produces one package per fixture.
//! Each is then trimmed to `{entry module} ∪ {SDK closure}` so a broken
//! fixture cannot poison another and both backends load the identical module
//! SET, differing only in the entry module's bytes.
//!
//! Direct backend: the fixture's `select`ed `.beam` bytes are spliced in as
//! the entry module over that same SDK closure (the capstone Deliverable-B
//! splice), so a trail divergence can only come from the entry module's two
//! byte productions.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::mir::select;
use aion_awl::{
    action_requirements, emit_artifact_in, schema_for_outcomes_in, schema_for_workflow_in,
};
use aion_package::{
    BeamModule, BeamSet, ExtractionLimits, Package, PackageBuilder, PackageOptions, package_project,
};
use serde_json::Value;

use crate::fixtures::Loaded;

/// One reference-buildable fixture, resolved to everything the project and its
/// `workflow.toml` need.
pub struct RefEntry {
    /// Covered-ratchet path (relative, no extension).
    pub name: String,
    /// Emitted Gleam module name (also the workflow type / manifest entry).
    pub entry_module: String,
    /// Emitted Gleam source.
    pub source: String,
    /// Generated-workflow sidecar (`project_metadata`), for child entries.
    pub awl_json: Value,
    /// Derived input JSON schema.
    pub input_schema: Value,
    /// Derived output JSON schema.
    pub output_schema: Value,
    /// Declared activity types (deduplicated, source order).
    pub activities: Vec<String>,
}

/// Resolves a lowering fixture to its reference-build inputs.
///
/// # Errors
///
/// Fails when emission or schema derivation fails (a reference-backend
/// refusal — recorded out-of-intersection by the caller, never a hard error).
pub fn ref_entry(loaded: &Loaded) -> Result<RefEntry, Box<dyn std::error::Error>> {
    let artifact = emit_artifact_in(&loaded.document, &loaded.dir)?;
    let input_schema = schema_for_workflow_in(&loaded.document, &loaded.dir)?;
    let output_schema = schema_for_outcomes_in(&loaded.document, &loaded.dir)?;
    let mut activities = Vec::new();
    let mut seen = HashSet::new();
    for requirement in action_requirements(&loaded.document) {
        if seen.insert(requirement.action.clone()) {
            activities.push(requirement.action);
        }
    }
    Ok(RefEntry {
        name: loaded.name.clone(),
        entry_module: artifact.entry_module.clone(),
        source: artifact.source.clone(),
        awl_json: artifact.project_metadata(),
        input_schema,
        output_schema,
        activities,
    })
}

/// The result of one batched reference build: the trimmed reference package
/// per fixture, keyed by ratchet name.
pub struct ReferenceBuild {
    packages: HashMap<String, Package>,
}

impl ReferenceBuild {
    /// The trimmed reference package for a fixture.
    pub fn package(&self, name: &str) -> Option<&Package> {
        self.packages.get(name)
    }
}

/// The repository root (two levels above this crate's manifest dir).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A stable scratch directory under the shared cargo target pile — never the
/// system temp dir (build artifacts in `/tmp` are banned).
fn scratch_dir(label: &str) -> PathBuf {
    repo_root().join("target/awl-test-scratch/bc4").join(label)
}

/// Builds every reference package in ONE Gleam project + ONE `gleam build`.
///
/// # Errors
///
/// A missing `gleam` CLI is a hard failure (never a skip — the example-build
/// law). A `gleam build` or packaging failure is likewise hard: a fixture that
/// cannot be emitted is filtered out UPSTREAM (recorded out-of-intersection),
/// so a failure here is a genuine reference-backend defect to surface.
pub fn build_reference(
    entries: &[RefEntry],
    label: &str,
) -> Result<ReferenceBuild, Box<dyn std::error::Error>> {
    let project = scratch_dir(&format!("reference_{label}"));
    let src = project.join("src");
    let schemas = project.join("schemas");
    let out = project.join("out");
    // Rebuild the source tree from scratch each run so a removed fixture never
    // lingers; the gleam `build/` cache under `project` survives for speed.
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&schemas);
    fs::create_dir_all(&src)?;
    fs::create_dir_all(&schemas)?;
    fs::create_dir_all(&out)?;
    write_gleam_toml(&project)?;
    write_workflow_toml(&project, entries)?;
    for entry in entries {
        fs::write(
            src.join(format!("{}.gleam", entry.entry_module)),
            &entry.source,
        )?;
        fs::write(
            src.join(format!("{}.awl.json", entry.entry_module)),
            serde_json::to_vec_pretty(&entry.awl_json)?,
        )?;
        fs::write(
            schemas.join(format!("{}__input.json", entry.entry_module)),
            serde_json::to_vec(&entry.input_schema)?,
        )?;
        fs::write(
            schemas.join(format!("{}__output.json", entry.entry_module)),
            serde_json::to_vec(&entry.output_schema)?,
        )?;
    }
    run_gleam_build(&project)?;
    let report = package_project(&project, &PackageOptions::default())?;

    let fixture_modules: HashSet<&str> = entries
        .iter()
        .map(|entry| entry.entry_module.as_str())
        .collect();
    let mut by_workflow_type: HashMap<&str, &Package> = HashMap::new();
    for packaged in &report.packages {
        by_workflow_type.insert(packaged.workflow_type.as_str(), &packaged.package);
    }

    let mut packages = HashMap::new();
    for entry in entries {
        let full = by_workflow_type
            .get(entry.entry_module.as_str())
            .ok_or_else(|| {
                format!(
                    "package_project produced no package for {} (entry module {})",
                    entry.name, entry.entry_module
                )
            })?;
        let trimmed = trim_to_closure(full, &entry.entry_module, &fixture_modules)?;
        packages.insert(entry.name.clone(), trimmed);
    }
    Ok(ReferenceBuild { packages })
}

/// Trims a project-wide package down to `{entry} ∪ {SDK closure}`: every beam
/// that is not some OTHER fixture's entry module. The result carries the same
/// manifest, so both backends run against the identical module set.
fn trim_to_closure(
    full: &Package,
    entry: &str,
    fixture_modules: &HashSet<&str>,
) -> Result<Package, Box<dyn std::error::Error>> {
    let mut modules = Vec::new();
    for beam in full.beams().iter() {
        let name = beam.name();
        if name == entry || !fixture_modules.contains(name) {
            modules.push(BeamModule::new(name, beam.bytes()));
        }
    }
    let archive =
        PackageBuilder::new(full.manifest().clone(), BeamSet::new(modules)?).write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// Splices the `select`ed direct bytes in as the entry module over the
/// reference package's SDK closure, returning the direct package.
///
/// # Errors
///
/// Fails when `lower`/`select` succeed but the direct module's internal name
/// disagrees with the manifest entry module (an IR-12 mangling divergence),
/// or when the archive cannot be built/reloaded.
pub fn splice_direct(
    reference: &Package,
    direct_bytes: &[u8],
) -> Result<Package, Box<dyn std::error::Error>> {
    let entry = reference.manifest().entry_module.clone();
    let mut modules = Vec::new();
    let mut replaced = false;
    for beam in reference.beams().iter() {
        if beam.name() == entry {
            modules.push(BeamModule::new(&entry, direct_bytes.to_vec()));
            replaced = true;
        } else {
            modules.push(BeamModule::new(beam.name(), beam.bytes()));
        }
    }
    if !replaced {
        return Err(format!("reference package has no entry beam named {entry}").into());
    }
    let archive = PackageBuilder::new(reference.manifest().clone(), BeamSet::new(modules)?)
        .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// Lowers + selects one fixture to its direct `.beam` bytes.
///
/// # Errors
///
/// Propagates the `select` error; `lower` refusals are classified upstream.
pub fn select_direct(
    module: &aion_awl::mir::MirModule,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(select(module)?)
}

fn write_gleam_toml(project: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let aion_flow = repo_root().join("gleam/aion_flow");
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_bc4_corpus\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n\
             [dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \
             \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            aion_flow.display()
        ),
    )?;
    Ok(())
}

fn write_workflow_toml(
    project: &Path,
    entries: &[RefEntry],
) -> Result<(), Box<dyn std::error::Error>> {
    let blocks: Vec<String> = entries
        .iter()
        .map(|entry| {
            let activities = entry
                .activities
                .iter()
                .map(|activity| format!("\"{activity}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "[[workflow]]\nentry_module = \"{module}\"\nentry_function = \"run\"\n\
                 timeout_seconds = 60\ninput_schema = \"schemas/{module}__input.json\"\n\
                 output_schema = \"schemas/{module}__output.json\"\nactivities = [{activities}]\n\
                 output = \"out/{module}.aion\"\n",
                module = entry.entry_module,
            )
        })
        .collect();
    fs::write(project.join("workflow.toml"), blocks.join("\n"))?;
    Ok(())
}

fn run_gleam_build(project: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("gleam")
        .arg("build")
        .current_dir(project)
        .output()
        .map_err(|error| {
            format!(
                "the BC-4 reference backend requires the `gleam` CLI on PATH \
                 (failed to spawn `gleam build`: {error}). This gate fails loudly \
                 by design — never a skip"
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "reference `gleam build` failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
