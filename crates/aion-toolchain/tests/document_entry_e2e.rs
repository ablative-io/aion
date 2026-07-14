//! Regressions for document-owned entry modules compiled from a built template.
//!
//! The production authoring template is commonly prebuilt. This test preserves
//! that shape and proves copied compiler output cannot enter a retargeted
//! document package or alter its content identity.

use std::path::{Path, PathBuf};
use std::process::Command;

use aion_toolchain::{CompileRequest, build_project, compile_source_for_entry};

const FROZEN_ENTRY: &str = "awl_hello";
const DOCUMENT_ENTRY: &str = "review_round";

const VALID_SOURCE: &str = r#"import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

pub fn run(raw_input: Dynamic) -> Result(String, Nil) {
  case decode.run(raw_input, decode.string) {
    Ok(name) -> Ok("Hello, " <> name)
    Error(_) -> Ok("Hello, world")
  }
}
"#;

type TestError = Box<dyn std::error::Error>;

fn gleam_binary() -> Option<PathBuf> {
    let candidate = PathBuf::from("gleam");
    match Command::new(&candidate).arg("--version").output() {
        Ok(output) if output.status.success() => Some(candidate),
        _ => None,
    }
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn provision_project(package_name: &str) -> Result<tempfile::TempDir, TestError> {
    let dir = tempfile::Builder::new()
        .prefix("aion-toolchain-document-entry-")
        .tempdir_in(examples_dir())?;
    let root = dir.path();
    std::fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"{package_name}\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"../../gleam/aion_flow\" }}\ngleam_stdlib = \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n"
        ),
    )?;
    std::fs::copy(
        examples_dir().join("awl-hello/manifest.toml"),
        root.join("manifest.toml"),
    )?;
    std::fs::write(
        root.join("workflow.toml"),
        format!(
            "[[workflow]]\nentry_module = \"{FROZEN_ENTRY}\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = []\noutput = \"fixture.aion\"\n"
        ),
    )?;
    std::fs::create_dir_all(root.join("schemas"))?;
    std::fs::write(root.join("schemas/input.json"), br#"{ "type": "string" }"#)?;
    std::fs::write(root.join("schemas/output.json"), br#"{ "type": "string" }"#)?;
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join(format!("src/{FROZEN_ENTRY}.gleam")), VALID_SOURCE)?;
    Ok(dir)
}

fn compile_document(
    project_root: &Path,
    gleam_path: &Path,
    entry_module: &str,
) -> Result<aion_toolchain::CompiledWorkflow, aion_toolchain::ToolchainError> {
    compile_source_for_entry(
        &CompileRequest {
            template_root: project_root,
            gleam_path,
            source: VALID_SOURCE,
        },
        entry_module,
    )
}

fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn explicit_entry_rebuilds_prebuilt_template_without_frozen_beams() -> Result<(), TestError> {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP explicit_entry_rebuilds_prebuilt_template_without_frozen_beams: `gleam` binary not runnable"
        );
        return Ok(());
    };
    let project = provision_project(FROZEN_ENTRY)?;
    build_project(project.path(), &gleam)?;
    let frozen_ebin = project
        .path()
        .join("build/dev/erlang")
        .join(FROZEN_ENTRY)
        .join("ebin");
    assert!(
        frozen_ebin.join(format!("{FROZEN_ENTRY}.beam")).is_file(),
        "the regression requires a prebuilt frozen entry BEAM"
    );
    assert!(
        frozen_ebin
            .join(format!("{FROZEN_ENTRY}@@main.beam"))
            .is_file(),
        "the regression requires Gleam's prebuilt root @@main BEAM"
    );

    let from_built = compile_document(project.path(), &gleam, DOCUMENT_ENTRY)?;
    assert_eq!(from_built.package.manifest().entry_module, DOCUMENT_ENTRY);
    assert!(from_built.package.beams().get(DOCUMENT_ENTRY).is_some());
    assert!(
        from_built.package.beams().get(FROZEN_ENTRY).is_none(),
        "the frozen entry BEAM must not enter the document package"
    );
    assert!(
        from_built
            .package
            .beams()
            .get(&format!("{FROZEN_ENTRY}@@main"))
            .is_none(),
        "the copied template @@main BEAM must not survive the clean root build"
    );
    assert!(
        from_built
            .package
            .beams()
            .get(&format!("{DOCUMENT_ENTRY}@@main"))
            .is_none(),
        "the document-owned @@main bootstrap is not workflow runtime code"
    );

    Ok(())
}

/// The compiler embeds the staged absolute path, so cross-workspace package
/// hashes are intentionally not compared here. The bounded invariant is that
/// templates differing only in their frozen package name compile the document
/// under one document-owned path, with neither frozen name in entry BEAM bytes.
#[test]
fn explicit_entry_removes_frozen_template_names_from_entry_beam() -> Result<(), TestError> {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP explicit_entry_removes_frozen_template_names_from_entry_beam: `gleam` binary not runnable"
        );
        return Ok(());
    };
    let first = provision_project("awl_hello")?;
    let second = provision_project("neutral_shell")?;
    build_project(first.path(), &gleam)?;
    build_project(second.path(), &gleam)?;

    let first_compiled = compile_document(first.path(), &gleam, DOCUMENT_ENTRY)?;
    let second_compiled = compile_document(second.path(), &gleam, DOCUMENT_ENTRY)?;
    for compiled in [&first_compiled, &second_compiled] {
        let entry = compiled
            .package
            .beams()
            .get(DOCUMENT_ENTRY)
            .ok_or("document package is missing its entry BEAM")?;
        for frozen_name in ["awl_hello", "neutral_shell"] {
            assert!(
                !contains_bytes(entry, frozen_name),
                "entry BEAM retained frozen package name `{frozen_name}`"
            );
        }
        assert!(
            contains_bytes(entry, "build/dev/erlang/review_round/"),
            "entry BEAM path must be owned by the document package"
        );
    }
    assert_eq!(
        first_compiled.package.manifest().entry_module,
        second_compiled.package.manifest().entry_module
    );
    Ok(())
}

#[test]
fn explicit_nested_entry_forms_package_under_canonical_identity() -> Result<(), TestError> {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP explicit_nested_entry_forms_package_under_canonical_identity: `gleam` binary not runnable"
        );
        return Ok(());
    };
    let project = provision_project(FROZEN_ENTRY)?;
    build_project(project.path(), &gleam)?;

    for requested in ["demo@nested", "demo/nested"] {
        let compiled = compile_document(project.path(), &gleam, requested)?;
        assert_eq!(compiled.workflow_type, "demo@nested");
        assert_eq!(compiled.package.manifest().entry_module, "demo@nested");
        assert!(
            compiled.package.beams().get("demo@nested").is_some(),
            "accepted form `{requested}` must package the compiled nested BEAM"
        );
    }
    Ok(())
}

#[test]
fn explicit_entry_refuses_dependency_package_name_collision() -> Result<(), TestError> {
    let project = provision_project(FROZEN_ENTRY)?;
    let result = compile_document(project.path(), Path::new("unused-gleam"), "gleam_json");
    assert!(
        matches!(
            result,
            Err(aion_toolchain::ToolchainError::InvalidProject { message })
                if message.contains("collides with a Gleam dependency")
        ),
        "a document-owned root package must never shadow a dependency"
    );
    Ok(())
}
