//! Local `generate` subcommand: declare-once activity codegen.
//!
//! Turns a package's typed activity declarations — the `manifest()` its
//! `src/<package>_activities.gleam` exports — into all the plumbing that must
//! agree byte-for-byte: the `<package>_io` types/codecs, the `<package>_codecs`
//! typed codec wrappers, the `<package>_activity_wrappers` constructors, the
//! per-tier worker module, and the remote wire-compat golden. The pure
//! generators live in `aion_package`; this command orchestrates the phases and
//! owns the one step the library cannot — running the Gleam toolchain to
//! execute `manifest()` and read back the declarations.
//!
//! The declarations are the single source of truth (ADR-014). Extracting them
//! means *running* `manifest()`, which means building the package — but the
//! workflow module imports the generated wrappers, which do not exist yet on a
//! fresh project or after the round-trip deletes them. So extraction first
//! renames aside (in place, restored on completion or error) every source
//! module that transitively imports the wrappers module, builds and runs a
//! throwaway probe against the activities module alone, then restores. Nothing
//! is written to a server; the whole command runs locally.
//!
//! `--check` compares every *generated* file against a fresh in-memory
//! regeneration and never writes one, but it is not side-effect-free: it still
//! drives the toolchain to read the declarations, so the same transient
//! rename-aside/probe/restore happens during the build.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_package::{
    ActivityDeclaration, CodegenMode, codegen_project, generate_activities, generate_codecs,
    parse_declarations,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use toml_edit::DocumentMut;

use crate::output::to_value;

/// Line the probe prints immediately before the manifest JSON, so the reader
/// is robust against any other stdout the toolchain emits.
const MANIFEST_MARKER: &str = "AION_MANIFEST_BEGIN";

/// Throwaway Gleam module the probe is written to. Chosen not to collide with a
/// plausible author module; created and deleted within one extraction.
const PROBE_MODULE: &str = "aion_generate_probe";

/// Suffix appended to a source file renamed aside for an extraction build.
/// Gleam ignores non-`.gleam` files, so the renamed module is excluded; the
/// suffix stays in the same directory (not moved to a temp tree) so a crash
/// leaves an obvious, recoverable file.
const ASIDE_SUFFIX: &str = ".aiongen-aside";

/// First line of the probe module, used to recognise a stale probe left by a
/// crashed prior run without risking a same-named author module.
const PROBE_DOC: &str =
    "//// Temporary manifest-extraction probe written by `aion generate`. Safe to delete.";

/// JSON document printed on stdout after a successful `generate` run.
#[derive(Serialize)]
struct GenerateOutput {
    /// Generated `src/<package>_io.gleam`, relative to the project root.
    io_module: String,
    /// Generated `src/<package>_codecs.gleam`, relative to the project root.
    codecs_module: String,
    /// Every generated activity file (wrappers, worker, golden), in order.
    activity_modules: Vec<String>,
    /// Number of activity declarations the package's `manifest()` returned.
    declarations: usize,
    /// `synced` when the workflow.toml activities list was rewritten,
    /// `unchanged` when it already matched, `checked` under `--check`.
    workflow_toml: &'static str,
    /// `written` for a generation run, `checked` for `--check`.
    action: &'static str,
}

/// Runs the `generate` subcommand: derives every per-activity artifact from the
/// package's typed declarations, or verifies them with `--check`.
pub(crate) fn run(path: &Path, check: bool) -> Result<Value> {
    let mode = if check {
        CodegenMode::Check
    } else {
        CodegenMode::Write
    };
    let package_name = read_gleam_package_name(path)?;

    // Phase 1-2: the schema-derived modules, which must exist on disk before
    // the package will build for declaration extraction.
    let io = codegen_project(path, mode)
        .with_context(|| format!("failed to generate the io module for {}", path.display()))?;
    let codecs = generate_codecs(path, mode).with_context(|| {
        format!(
            "failed to generate the codecs module for {}",
            path.display()
        )
    })?;

    // Phase 3: run the package's `manifest()` to read the declarations.
    let manifest_json = extract_declarations(path, &package_name)?;
    let declarations = parse_declarations(&manifest_json).with_context(|| {
        format!(
            "failed to parse the activity manifest emitted by {package_name}_activities.manifest()"
        )
    })?;

    // Phase 4: the declaration-derived plumbing.
    let activities = generate_activities(path, &declarations, mode).with_context(|| {
        format!(
            "failed to generate the activity plumbing for {}",
            path.display()
        )
    })?;

    // Phase 5: keep the workflow.toml activities list in step with the
    // declared names.
    let workflow_toml = sync_workflow_activities(path, &declarations, mode)?;

    to_value(GenerateOutput {
        io_module: io.module_relative,
        codecs_module: codecs.module_relative,
        activity_modules: activities
            .artifacts
            .iter()
            .map(|artifact| artifact.relative.clone())
            .collect(),
        declarations: declarations.len(),
        workflow_toml,
        action: if check { "checked" } else { "written" },
    })
}

/// Reads the package name from the project's `gleam.toml`.
fn read_gleam_package_name(root: &Path) -> Result<String> {
    let gleam_toml = root.join("gleam.toml");
    let contents = fs::read_to_string(&gleam_toml)
        .with_context(|| format!("failed to read {}", gleam_toml.display()))?;
    let document = contents
        .parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", gleam_toml.display()))?;
    document
        .get("name")
        .and_then(|name| name.as_str())
        .map(str::to_owned)
        .with_context(|| format!("{} has no `name` field", gleam_toml.display()))
}

/// Builds and runs a probe that prints `manifest()` as JSON, returning the raw
/// JSON bytes. Source modules that transitively import the generated wrappers
/// are renamed aside for the build so the package compiles without them, and
/// restored before this returns (or on error).
fn extract_declarations(root: &Path, package_name: &str) -> Result<Vec<u8>> {
    let src = root.join("src");
    let activities_module = format!("{package_name}_activities");
    let activities_file = src.join(format!("{activities_module}.gleam"));
    if !activities_file.is_file() {
        bail!(
            "expected {} to exist and export `pub fn manifest() -> List(activity.Declaration)`; \
             the declarations are the single source of truth `aion generate` reads",
            activities_file.display()
        );
    }

    // A prior extraction killed mid-flight (SIGKILL / power loss, which no
    // `Drop` can cover) can leave renamed-aside modules and a stale probe in
    // the tree. Recover them before touching anything, so the build sees the
    // real sources.
    recover_leftover_scratch(&src)?;

    let wrappers_module = format!("{package_name}_activity_wrappers");
    let blocking = modules_reaching(&src, &wrappers_module)?;

    let mut scratch = ExtractionScratch::default();
    for module_file in blocking {
        scratch.rename_aside(&module_file)?;
    }
    scratch.write_probe(&src, &activities_module)?;

    let output = Command::new("gleam")
        .args(["run", "-m", PROBE_MODULE])
        .current_dir(root)
        .output()
        .with_context(|| {
            format!(
                "failed to run `gleam run -m {PROBE_MODULE}` in {}",
                root.display()
            )
        })?;
    // `scratch` restores the renamed modules and removes the probe when it
    // drops at the end of this function, including on the bail! paths below.
    if !output.status.success() {
        bail!(
            "manifest extraction failed: `gleam run -m {PROBE_MODULE}` exited with {} in {}\n{}",
            output.status,
            root.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    extract_marked_json(&output.stdout)
}

/// Returns the source files of every module under `src` that transitively
/// imports `target` (the to-be-generated wrappers module), so they can be set
/// aside while the package is built for declaration extraction.
fn modules_reaching(src: &Path, target: &str) -> Result<Vec<PathBuf>> {
    let mut imports: HashMap<String, (PathBuf, Vec<String>)> = HashMap::new();
    collect_modules(src, src, &mut imports)?;

    let mut blocking = Vec::new();
    for (module, (file, _)) in &imports {
        if reaches(module, target, &imports, &mut Vec::new()) {
            blocking.push(file.clone());
        }
    }
    // Deterministic order so the rename/restore sequence is reproducible.
    blocking.sort();
    Ok(blocking)
}

/// Walks `dir` recursively, recording each `*.gleam` module's name and the
/// modules it imports.
fn collect_modules(
    src_root: &Path,
    dir: &Path,
    imports: &mut HashMap<String, (PathBuf, Vec<String>)>,
) -> Result<()> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to list source directory {}", dir.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read an entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_modules(src_root, &path, imports)?;
        } else if path.extension().is_some_and(|ext| ext == "gleam") {
            let module = module_name(src_root, &path);
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            imports.insert(module, (path, parse_imports(&contents)));
        }
    }
    Ok(())
}

/// Derives a Gleam module name from a source file path: the path relative to
/// `src`, without the `.gleam` extension, with directory separators as `/`.
fn module_name(src_root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(src_root).unwrap_or(file);
    let without_ext = relative.with_extension("");
    without_ext
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Extracts the imported module names from Gleam source: the token after
/// `import`, up to the first `.` (the `.{..}` unqualified list) or whitespace
/// (an `as` alias).
fn parse_imports(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("import ")?;
            let token = rest
                .split_whitespace()
                .next()?
                .split('.')
                .next()
                .unwrap_or_default();
            (!token.is_empty()).then(|| token.to_owned())
        })
        .collect()
}

/// Whether `module` reaches `target` by following local imports, guarding
/// against import cycles.
fn reaches(
    module: &str,
    target: &str,
    imports: &HashMap<String, (PathBuf, Vec<String>)>,
    stack: &mut Vec<String>,
) -> bool {
    let Some((_, module_imports)) = imports.get(module) else {
        return false;
    };
    if module_imports.iter().any(|import| import == target) {
        return true;
    }
    if stack.iter().any(|seen| seen == module) {
        return false;
    }
    stack.push(module.to_owned());
    let found = module_imports
        .iter()
        .any(|import| reaches(import, target, imports, stack));
    stack.pop();
    found
}

/// Pulls the manifest JSON line that follows the marker out of the probe's
/// stdout.
fn extract_marked_json(stdout: &[u8]) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() == MANIFEST_MARKER {
            let json = lines.next().with_context(|| {
                "manifest probe printed its marker but no JSON line followed".to_owned()
            })?;
            return Ok(json.trim().as_bytes().to_vec());
        }
    }
    bail!("manifest probe did not print the `{MANIFEST_MARKER}` marker; stdout was:\n{text}")
}

/// The Gleam source of the manifest-extraction probe.
fn probe_source(activities_module: &str) -> String {
    format!(
        "{PROBE_DOC}\n\n\
         import aion/manifest\n\
         import gleam/io\n\
         import {activities_module} as activities\n\n\
         pub fn main() {{\n  \
         io.println(\"{MANIFEST_MARKER}\")\n  \
         io.println(manifest.to_json(activities.manifest()))\n}}\n"
    )
}

/// Recovers the on-disk leftovers of a prior extraction that was killed before
/// its [`ExtractionScratch`] could restore them: renames every `*.aiongen-aside`
/// module back (or removes it when the original already exists), and removes a
/// stale probe module. Idempotent and safe on a clean tree.
fn recover_leftover_scratch(src: &Path) -> Result<()> {
    recover_asides(src)?;
    let probe = src.join(format!("{PROBE_MODULE}.gleam"));
    match fs::read_to_string(&probe) {
        Ok(contents) if contents.starts_with(PROBE_DOC) => fs::remove_file(&probe)
            .with_context(|| format!("failed to remove a stale probe {}", probe.display()))?,
        _ => {}
    }
    Ok(())
}

/// Walks `dir` for files renamed aside by a crashed extraction and restores
/// each: rename back when the original is gone, or drop the stale copy when the
/// original is present (the original is authoritative).
fn recover_asides(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to list {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("failed to read an entry in {}", dir.display()))?
            .path();
        if path.is_dir() {
            recover_asides(&path)?;
        } else if let Some(original) = aside_original(&path) {
            if original.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove stale aside {}", path.display()))?;
            } else {
                fs::rename(&path, &original).with_context(|| {
                    format!(
                        "failed to restore {} from a crashed extraction",
                        original.display()
                    )
                })?;
            }
        }
    }
    Ok(())
}

/// If `path` ends in the aside suffix, returns the original path it was renamed
/// from.
fn aside_original(path: &Path) -> Option<PathBuf> {
    let name = path.to_str()?;
    name.strip_suffix(ASIDE_SUFFIX).map(PathBuf::from)
}

/// Tracks the temporary on-disk changes one extraction makes — the probe module
/// it writes and the source modules it renames aside — and undoes them when it
/// drops, so a build failure or panic never leaves the project mangled.
#[derive(Default)]
struct ExtractionScratch {
    probe: Option<PathBuf>,
    /// `(aside_path, original_path)` pairs to rename back.
    renamed: Vec<(PathBuf, PathBuf)>,
}

impl ExtractionScratch {
    /// Renames `original` aside (appending `.aiongen-aside`, which Gleam
    /// ignores) so the build excludes it.
    fn rename_aside(&mut self, original: &Path) -> Result<()> {
        let mut aside_name: OsString = original.as_os_str().to_owned();
        aside_name.push(ASIDE_SUFFIX);
        let aside = PathBuf::from(aside_name);
        fs::rename(original, &aside).with_context(|| {
            format!(
                "failed to set aside {} for manifest extraction",
                original.display()
            )
        })?;
        self.renamed.push((aside, original.to_path_buf()));
        Ok(())
    }

    /// Writes the probe module into `src`.
    fn write_probe(&mut self, src: &Path, activities_module: &str) -> Result<()> {
        let probe = src.join(format!("{PROBE_MODULE}.gleam"));
        fs::write(&probe, probe_source(activities_module))
            .with_context(|| format!("failed to write the probe module {}", probe.display()))?;
        self.probe = Some(probe);
        Ok(())
    }
}

impl Drop for ExtractionScratch {
    fn drop(&mut self) {
        if let Some(probe) = self.probe.take() {
            if let Err(error) = fs::remove_file(&probe) {
                if probe.exists() {
                    eprintln!(
                        "warning: failed to remove manifest probe {}: {error}",
                        probe.display()
                    );
                }
            }
        }
        for (aside, original) in self.renamed.drain(..) {
            if let Err(error) = fs::rename(&aside, &original) {
                eprintln!(
                    "error: failed to restore {} from {} after manifest extraction: {error}; \
                     rename it back manually",
                    original.display(),
                    aside.display()
                );
            }
        }
    }
}

/// Keeps every `[[workflow]]` table's `activities` list in step with the
/// declared activity names. Returns the action taken for the printed report.
///
/// The comparison is by name and order, not bytes: a workflow.toml whose list
/// already matches is left untouched (its formatting and comments are
/// preserved), so only an actually-stale list is a `--check` failure or a
/// rewrite.
fn sync_workflow_activities(
    root: &Path,
    declarations: &[ActivityDeclaration],
    mode: CodegenMode,
) -> Result<&'static str> {
    let toml_path = root.join("workflow.toml");
    let original = fs::read_to_string(&toml_path)
        .with_context(|| format!("failed to read {}", toml_path.display()))?;
    let mut document = original
        .parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", toml_path.display()))?;

    let desired: Vec<&str> = declarations
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect();

    let workflows = document
        .get_mut("workflow")
        .and_then(|item| item.as_array_of_tables_mut())
        .with_context(|| {
            format!(
                "{} declares no [[workflow]] entry to attach activities to",
                toml_path.display()
            )
        })?;

    // A package-level `manifest()` cannot attribute its activities across more
    // than one workflow, so syncing the same list into every `[[workflow]]`
    // would be a guess. Refuse rather than guess; per-workflow attribution is
    // out of scope for WA-001 (the `.aion` format is single-workflow today).
    if workflows.len() > 1 {
        bail!(
            "{} declares {} [[workflow]] entries; `aion generate` syncs the activities \
             list for a single-workflow package only",
            toml_path.display(),
            workflows.len()
        );
    }

    let mut changed = false;
    for table in workflows.iter_mut() {
        if workflow_activities(table) == desired {
            continue;
        }
        if mode == CodegenMode::Check {
            bail!(
                "--check failed: {} activities list is out of date; run `aion generate`",
                toml_path.display()
            );
        }
        let mut array = toml_edit::Array::new();
        for name in &desired {
            array.push(*name);
        }
        table["activities"] = toml_edit::value(array);
        changed = true;
    }

    if mode == CodegenMode::Check {
        return Ok("checked");
    }
    if changed {
        fs::write(&toml_path, document.to_string())
            .with_context(|| format!("failed to write {}", toml_path.display()))?;
        Ok("synced")
    } else {
        Ok("unchanged")
    }
}

/// Reads a `[[workflow]]` table's `activities` list as owned strings, treating a
/// missing or malformed list as empty.
fn workflow_activities(table: &toml_edit::Table) -> Vec<String> {
    table
        .get("activities")
        .and_then(|item| item.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{extract_marked_json, module_name, parse_imports, reaches};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn module_names_use_forward_slashes_under_src() {
        let src = Path::new("/p/src");
        assert_eq!(
            module_name(src, Path::new("/p/src/order_saga.gleam")),
            "order_saga"
        );
        assert_eq!(
            module_name(src, Path::new("/p/src/order_saga/locals.gleam")),
            "order_saga/locals"
        );
    }

    #[test]
    fn imports_are_parsed_to_bare_module_names() {
        let source = "import aion/manifest\nimport gleam/io.{println}\n\
                      import order_saga_codecs as codecs\nlet x = 1\n  import nested/mod\n";
        assert_eq!(
            parse_imports(source),
            vec![
                "aion/manifest".to_owned(),
                "gleam/io".to_owned(),
                "order_saga_codecs".to_owned(),
                "nested/mod".to_owned(),
            ]
        );
    }

    #[test]
    fn transitive_importers_of_the_target_are_found() {
        // workflow -> wrappers (direct); mid -> workflow (transitive);
        // activities -> codecs (never reaches wrappers).
        let mut imports: HashMap<String, (PathBuf, Vec<String>)> = HashMap::new();
        imports.insert(
            "workflow".to_owned(),
            (PathBuf::from("workflow.gleam"), vec!["wrappers".to_owned()]),
        );
        imports.insert(
            "mid".to_owned(),
            (PathBuf::from("mid.gleam"), vec!["workflow".to_owned()]),
        );
        imports.insert(
            "activities".to_owned(),
            (PathBuf::from("activities.gleam"), vec!["codecs".to_owned()]),
        );

        assert!(reaches("workflow", "wrappers", &imports, &mut Vec::new()));
        assert!(reaches("mid", "wrappers", &imports, &mut Vec::new()));
        assert!(!reaches(
            "activities",
            "wrappers",
            &imports,
            &mut Vec::new()
        ));
    }

    #[test]
    fn cyclic_imports_do_not_loop() {
        let mut imports: HashMap<String, (PathBuf, Vec<String>)> = HashMap::new();
        imports.insert(
            "a".to_owned(),
            (PathBuf::from("a.gleam"), vec!["b".to_owned()]),
        );
        imports.insert(
            "b".to_owned(),
            (PathBuf::from("b.gleam"), vec!["a".to_owned()]),
        );
        // Neither reaches the target; the cycle must terminate.
        assert!(!reaches("a", "wrappers", &imports, &mut Vec::new()));
    }

    #[test]
    fn marked_json_is_extracted_from_noisy_stdout() -> Result<(), Box<dyn std::error::Error>> {
        let stdout =
            b"  Compiling demo\n   Compiled in 0.3s\nAION_MANIFEST_BEGIN\n[{\"name\":\"a\"}]\n";
        assert_eq!(extract_marked_json(stdout)?, b"[{\"name\":\"a\"}]");
        Ok(())
    }

    #[test]
    fn missing_marker_is_an_error() {
        let stdout = b"some unrelated output\n";
        assert!(extract_marked_json(stdout).is_err());
    }

    #[test]
    fn crash_leftovers_are_recovered() -> Result<(), Box<dyn std::error::Error>> {
        use super::{PROBE_MODULE, recover_leftover_scratch};

        let temp = tempfile::tempdir()?;
        let src = temp.path();
        std::fs::create_dir(src.join("nested"))?;
        // A module renamed aside with its original gone → restored.
        std::fs::write(src.join("order_saga.gleam.aiongen-aside"), b"workflow")?;
        // A renamed-aside copy whose original survived → the stale copy dropped.
        std::fs::write(src.join("nested/helper.gleam"), b"current")?;
        std::fs::write(src.join("nested/helper.gleam.aiongen-aside"), b"stale")?;
        // A stale probe (recognised by its header) → removed; a same-named
        // author module would be left alone.
        std::fs::write(
            src.join(format!("{PROBE_MODULE}.gleam")),
            super::probe_source("demo_activities"),
        )?;

        recover_leftover_scratch(src)?;

        assert!(src.join("order_saga.gleam").is_file());
        assert!(!src.join("order_saga.gleam.aiongen-aside").exists());
        assert_eq!(std::fs::read(src.join("nested/helper.gleam"))?, b"current");
        assert!(!src.join("nested/helper.gleam.aiongen-aside").exists());
        assert!(!src.join(format!("{PROBE_MODULE}.gleam")).exists());

        // Idempotent on a clean tree.
        recover_leftover_scratch(src)?;
        Ok(())
    }
}
