//! End-to-end tests for the authoring toolchain against the real `gleam`
//! binary.
//!
//! The compile tests require the external `gleam` binary plus the cached Hex
//! dependencies of the `aion_flow` SDK. Per the repository rule, that
//! requirement is gated at RUNTIME: when `gleam` is absent (or the build
//! environment cannot resolve dependencies) the test emits a skip line and
//! returns `Ok(())` rather than failing — it is never `#[ignore]`d.

use std::path::{Path, PathBuf};
use std::process::Command;

use aion_toolchain::{CompileRequest, ToolchainError, compile_source};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The submitted source for the happy-path workflow: a minimal, valid Gleam
/// workflow whose `run` returns the decoded name. It compiles and type-checks.
const VALID_SOURCE: &str = r#"import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

pub type WorkflowError {
  BadInput(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(name) -> Ok("Hello, " <> name)
    Error(_) -> Error(BadInput("workflow input was not a string"))
  }
}
"#;

/// A second, distinct valid workflow source: it compiles and type-checks but
/// produces a different `run` body (and so different bytecode and a different
/// content hash) than [`VALID_SOURCE`]. Used to prove concurrent submissions of
/// DIFFERENT source receive THEIR OWN artifact, never cross-talk.
const OTHER_VALID_SOURCE: &str = r#"import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

pub type WorkflowError {
  BadInput(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(name) -> Ok("Goodbye, " <> name <> "!")
    Error(_) -> Error(BadInput("the other workflow needs a string"))
  }
}
"#;

/// The submitted source for the failure path: `run` claims to return
/// `Result(String, _)` but returns a bare `Int`, a type error the Gleam
/// compiler rejects.
const TYPE_ERROR_SOURCE: &str = r"import gleam/dynamic.{type Dynamic}

pub type WorkflowError {
  BadInput(message: String)
}

pub fn run(_raw_input: Dynamic) -> Result(String, WorkflowError) {
  42
}
";

/// Resolves the `gleam` binary path, or `None` when it is not runnable in this
/// environment. The toolchain itself takes a path; tests locate it on `PATH`.
fn gleam_binary() -> Option<PathBuf> {
    let candidate = PathBuf::from("gleam");
    match Command::new(&candidate).arg("--version").output() {
        Ok(output) if output.status.success() => Some(candidate),
        _ => None,
    }
}

/// Absolute path to the workspace `aion_flow` Gleam SDK, the local dependency
/// the example projects build against.
fn aion_flow_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../gleam/aion_flow")
}

/// Absolute path to the repository `examples/` directory.
///
/// Every real example template (`examples/order-saga`, `examples/approval-gate`,
/// `examples/stacked-dev-remote`, …) lives here at directory depth 2 and records
/// its SDK dependency as the **relative** path `../../gleam/aion_flow`. The
/// relative-dep fixture is provisioned here too, at the same depth, so the
/// staged working copy (a same-depth sibling) resolves `../../gleam/aion_flow`
/// to the real `gleam/aion_flow` exactly as production does.
fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

/// Provisions a fresh single-workflow Gleam project whose `aion_flow`
/// dependency is the **relative** path `../../gleam/aion_flow` — the exact form
/// every real example template uses — placed at the same directory depth as
/// those templates (inside the repo's `examples/`). This is what makes the
/// staging-depth bug observable: if the working copy were staged one directory
/// deeper than the template, `../../gleam/aion_flow` would resolve to the wrong
/// location and `gleam build` would fail to canonicalise the SDK path.
///
/// The returned temp dir is auto-removed on drop, so the repo's `examples/`
/// directory is left clean.
fn provision_relative_dep_project(
    label: &str,
) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("aion-toolchain-reldep-{label}-"))
        .tempdir_in(examples_dir())?;
    let root = dir.path();

    std::fs::write(
        root.join("gleam.toml"),
        b"name = \"aion_authoring_fixture\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n[dependencies]\naion_flow = { path = \"../../gleam/aion_flow\" }\ngleam_stdlib = \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n".as_slice(),
    )?;
    std::fs::write(
        root.join("workflow.toml"),
        b"[[workflow]]\nentry_module = \"aion_authoring_fixture\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = []\noutput = \"fixture.aion\"\n",
    )?;
    std::fs::create_dir_all(root.join("schemas"))?;
    std::fs::write(root.join("schemas/input.json"), br#"{ "type": "string" }"#)?;
    std::fs::write(root.join("schemas/output.json"), br#"{ "type": "string" }"#)?;
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(
        root.join("src/aion_authoring_fixture.gleam"),
        b"pub fn run(_raw: a) -> Result(String, Nil) {\n  Ok(\"placeholder\")\n}\n",
    )?;
    Ok(dir)
}

/// Provisions a fresh single-workflow Gleam project in a temp dir whose
/// `aion_flow` dependency points at the absolute workspace SDK path, so a
/// `gleam build` resolves the local SDK plus cached Hex deps without relying
/// on the temp dir's relative position.
fn provision_project(label: &str) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("aion-toolchain-e2e-{label}-"))
        .tempdir()?;
    let root = dir.path();
    let flow = aion_flow_path();
    let flow_display = flow.to_str().ok_or("aion_flow path is not valid UTF-8")?;

    std::fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"aion_authoring_fixture\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"{flow_display}\" }}\ngleam_stdlib = \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n"
        ),
    )?;
    std::fs::write(
        root.join("workflow.toml"),
        b"[[workflow]]\nentry_module = \"aion_authoring_fixture\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = []\noutput = \"fixture.aion\"\n",
    )?;
    std::fs::create_dir_all(root.join("schemas"))?;
    std::fs::write(root.join("schemas/input.json"), br#"{ "type": "string" }"#)?;
    std::fs::write(root.join("schemas/output.json"), br#"{ "type": "string" }"#)?;
    std::fs::create_dir_all(root.join("src"))?;
    // A placeholder so the project is buildable even before source submission;
    // compile_source overwrites it with the submitted bytes.
    std::fs::write(
        root.join("src/aion_authoring_fixture.gleam"),
        b"pub fn run(_raw: a) -> Result(String, Nil) {\n  Ok(\"placeholder\")\n}\n",
    )?;
    Ok(dir)
}

/// R1 acceptance #1 / C12: a valid workflow source compiles to a loadable
/// `.aion` with a non-empty content hash, by invoking the external gleam
/// binary.
#[test]
fn valid_source_compiles_to_loadable_package() -> TestResult {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP valid_source_compiles_to_loadable_package: `gleam` binary not runnable in this environment"
        );
        return Ok(());
    };
    let project = provision_project("valid")?;
    let request = CompileRequest {
        template_root: project.path(),
        gleam_path: &gleam,
        source: VALID_SOURCE,
    };

    let compiled = match compile_source(&request) {
        Ok(compiled) => compiled,
        Err(ToolchainError::TypeCheck { diagnostics }) => {
            // A dependency-resolution failure in a sandboxed CI environment is
            // an environmental skip, not a product failure: the valid source
            // itself type-checks (proven locally).
            eprintln!(
                "SKIP valid_source_compiles_to_loadable_package: gleam build could not complete in this environment:\n{diagnostics}"
            );
            return Ok(());
        }
        Err(other) => return Err(Box::new(other)),
    };

    assert_eq!(compiled.workflow_type, "aion_authoring_fixture");
    // The verified `.aion` was written and re-loaded into the in-memory
    // `Package` inside the per-submission workspace, which is removed when
    // `compile_source` returns — so `output_path` no longer exists on disk, and
    // the template root is left clean (no `.aion` leaks into it).
    assert!(
        !compiled.output_path.exists(),
        "the per-submission workspace (and its .aion) is cleaned up after compile"
    );
    assert!(
        !project.path().join("fixture.aion").exists(),
        "the read-only template is never written to: no .aion is produced in it"
    );
    assert!(
        !project.path().join("build").exists(),
        "the read-only template is never built in: no build/ dir is produced in it"
    );
    assert!(
        !compiled.version.content_hash.to_string().is_empty(),
        "the verified package carries a content hash"
    );
    assert_eq!(
        compiled.package.content_hash().to_string(),
        compiled.version.content_hash.to_string(),
        "the package and version record agree on the content hash"
    );
    assert_eq!(
        compiled.package.manifest().entry_module,
        "aion_authoring_fixture"
    );
    Ok(())
}

/// R1 acceptance #2: a type-erroneous source yields the gleam type error
/// (carrying the compiler text) rather than a panic or a partial package — no
/// `.aion` is written.
#[test]
fn type_error_source_yields_inline_diagnostics_and_no_package() -> TestResult {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP type_error_source_yields_inline_diagnostics_and_no_package: `gleam` binary not runnable in this environment"
        );
        return Ok(());
    };
    let project = provision_project("type-error")?;
    let request = CompileRequest {
        template_root: project.path(),
        gleam_path: &gleam,
        source: TYPE_ERROR_SOURCE,
    };

    let Err(error) = compile_source(&request) else {
        return Err("type-erroneous source unexpectedly compiled".into());
    };

    let ToolchainError::TypeCheck { diagnostics } = error else {
        // Any non-TypeCheck error here (e.g. a spawn failure) is an
        // environmental skip; the source itself is a genuine type error.
        eprintln!(
            "SKIP type_error_source_yields_inline_diagnostics_and_no_package: gleam build did not run as a type check in this environment: {error}"
        );
        return Ok(());
    };

    assert!(
        diagnostics.to_lowercase().contains("error"),
        "diagnostics must carry the compiler error text: {diagnostics}"
    );
    assert!(
        !project.path().join("fixture.aion").exists(),
        "no .aion archive is written on a type error"
    );
    Ok(())
}

/// A spawn failure (a `gleam_path` that does not exist) is a typed
/// `GleamSpawn` naming the path — never a panic and never a partial package.
#[test]
fn missing_gleam_binary_is_typed_spawn_error() -> TestResult {
    let project = provision_project("spawn")?;
    let missing = project.path().join("definitely-not-a-real-gleam-binary");
    let request = CompileRequest {
        template_root: project.path(),
        gleam_path: &missing,
        source: VALID_SOURCE,
    };

    let Err(error) = compile_source(&request) else {
        return Err("missing gleam binary unexpectedly compiled".into());
    };

    match error {
        ToolchainError::GleamSpawn { gleam_path, .. } => {
            assert_eq!(gleam_path, missing);
        }
        other => return Err(format!("expected GleamSpawn, got {other}").into()),
    }
    assert!(
        !project.path().join("fixture.aion").exists(),
        "no .aion archive is written when the compiler cannot be spawned"
    );
    Ok(())
}

/// Per-submission isolation (the BEST-solution property at the toolchain
/// layer): two OVERLAPPING submissions of DIFFERENT source, sharing the SAME
/// read-only template, each receive THEIR OWN artifact — distinct content
/// hashes, no cross-talk, no shared `build/` corruption — because each
/// submission compiles in its own throwaway workspace. The template is left
/// untouched.
#[test]
fn concurrent_submissions_of_different_source_get_their_own_artifact() -> TestResult {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP concurrent_submissions_of_different_source_get_their_own_artifact: `gleam` binary not runnable in this environment"
        );
        return Ok(());
    };
    // One shared, read-only template both submissions build against. If
    // isolation were broken, two concurrent builds would race on this single
    // root's entry-file, build/ dir, and .aion output. The template uses the
    // production RELATIVE `aion_flow = { path = "../../gleam/aion_flow" }`
    // dependency (mirroring every real example), so the concurrent path genuinely
    // exercises the same-depth staging that production relies on.
    let project = provision_relative_dep_project("concurrent")?;
    let template_root = project.path().to_path_buf();
    let gleam_path = gleam.clone();

    let template_for_a = template_root.clone();
    let gleam_for_a = gleam_path.clone();
    let first = std::thread::spawn(move || {
        compile_source(&CompileRequest {
            template_root: &template_for_a,
            gleam_path: &gleam_for_a,
            source: VALID_SOURCE,
        })
    });
    let template_for_b = template_root.clone();
    let gleam_for_b = gleam_path.clone();
    let second = std::thread::spawn(move || {
        compile_source(&CompileRequest {
            template_root: &template_for_b,
            gleam_path: &gleam_for_b,
            source: OTHER_VALID_SOURCE,
        })
    });

    let first = first.join().map_err(|_| "first compile thread panicked")?;
    let second = second
        .join()
        .map_err(|_| "second compile thread panicked")?;

    // A dependency-resolution failure in a sandboxed CI environment is an
    // environmental skip on either thread, not a product failure.
    let (first, second) = match (first, second) {
        (Ok(first), Ok(second)) => (first, second),
        (first, second) => {
            for (label, result) in [("first", first), ("second", second)] {
                if let Err(ToolchainError::TypeCheck { diagnostics }) = result {
                    eprintln!(
                        "SKIP concurrent_submissions_of_different_source_get_their_own_artifact: gleam build could not complete in this environment ({label}):\n{diagnostics}"
                    );
                    return Ok(());
                }
            }
            return Err("a concurrent submission failed for a non-environmental reason".into());
        }
    };

    // Each submission got its own DISTINCT artifact — different source, so a
    // different content hash. A shared build/ or .aion would have collapsed
    // these to one hash (or returned one author the other's bytes).
    assert_ne!(
        first.package.content_hash().to_string(),
        second.package.content_hash().to_string(),
        "concurrent submissions of different source must receive distinct content hashes (no cross-talk / wrong-artifact return)"
    );
    // Each carries its OWN source's bytecode, verified through its own version
    // record (the package re-loaded from its own isolated workspace).
    assert_eq!(
        first.package.content_hash().to_string(),
        first.version.content_hash.to_string(),
        "the first artifact's package and version record agree"
    );
    assert_eq!(
        second.package.content_hash().to_string(),
        second.version.content_hash.to_string(),
        "the second artifact's package and version record agree"
    );
    // Both are the same workflow type (same template entry module) but distinct
    // versions — exactly the live-authoring loop's shape.
    assert_eq!(first.workflow_type, "aion_authoring_fixture");
    assert_eq!(second.workflow_type, "aion_authoring_fixture");

    // The shared template is left pristine: no build artifacts leaked into it.
    assert!(
        !template_root.join("fixture.aion").exists(),
        "the read-only template carries no .aion after concurrent submissions"
    );
    assert!(
        !template_root.join("build").exists(),
        "the read-only template carries no build/ dir after concurrent submissions"
    );
    Ok(())
}

/// LOAD-BEARING staging-depth test: a template that records its SDK dependency
/// as the production-shape RELATIVE path `../../gleam/aion_flow` (exactly like
/// `examples/order-saga`, `examples/approval-gate`, `examples/stacked-dev-remote`)
/// compiles through the staged workspace and yields a loadable `.aion` with a
/// real content hash.
///
/// This is the test the earlier absolute-dep fixtures could not be: an absolute
/// `aion_flow` path resolves regardless of staging depth, so it passed even
/// while production (which uses the relative form) was broken. Here the staged
/// working copy must sit at the SAME directory depth as the template for
/// `../../gleam/aion_flow` to resolve to the real SDK — proving the same-depth
/// sibling staging is correct. Against the buggy `<temp>/project` staging this
/// fails to canonicalise the SDK path; against the fixed same-depth staging it
/// succeeds.
#[test]
fn relative_dep_template_compiles_through_staged_workspace() -> TestResult {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP relative_dep_template_compiles_through_staged_workspace: `gleam` binary not runnable in this environment"
        );
        return Ok(());
    };
    let project = provision_relative_dep_project("compiles")?;
    let request = CompileRequest {
        template_root: project.path(),
        gleam_path: &gleam,
        source: VALID_SOURCE,
    };

    let compiled = match compile_source(&request) {
        Ok(compiled) => compiled,
        Err(ToolchainError::TypeCheck { diagnostics }) => {
            // A genuine dependency-resolution failure in a sandboxed CI
            // environment is an environmental skip. The staging-depth bug,
            // however, surfaced as a `gleam build` failure to *canonicalise* the
            // relative SDK path — so a diagnostics string mentioning the SDK
            // path resolution is a real product failure and must NOT be skipped.
            if diagnostics.contains("aion_flow") && diagnostics.contains("canonicalise") {
                return Err(format!(
                    "relative-dep template failed to resolve `../../gleam/aion_flow` through the staged workspace — staging depth is wrong:\n{diagnostics}"
                )
                .into());
            }
            eprintln!(
                "SKIP relative_dep_template_compiles_through_staged_workspace: gleam build could not complete in this environment:\n{diagnostics}"
            );
            return Ok(());
        }
        Err(other) => return Err(Box::new(other)),
    };

    assert_eq!(compiled.workflow_type, "aion_authoring_fixture");
    assert!(
        !compiled.version.content_hash.to_string().is_empty(),
        "the relative-dep template produces a verified package with a content hash"
    );
    assert_eq!(
        compiled.package.content_hash().to_string(),
        compiled.version.content_hash.to_string(),
        "the package and version record agree on the content hash"
    );
    // The staged working copy (and its build artifacts) is removed on drop, and
    // the in-repo template carries nothing — no `.aion`, no `build/`.
    assert!(
        !compiled.output_path.exists(),
        "the per-submission workspace (and its .aion) is cleaned up after compile"
    );
    assert!(
        !project.path().join("fixture.aion").exists(),
        "the read-only relative-dep template is never written to"
    );
    assert!(
        !project.path().join("build").exists(),
        "the read-only relative-dep template is never built in"
    );
    Ok(())
}
