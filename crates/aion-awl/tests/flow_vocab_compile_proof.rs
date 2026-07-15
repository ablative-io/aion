//! Real `gleam build` compile proof for the flow-vocabulary B1 fixture:
//! a document exercising raw strings, a `json { … }` literal, const
//! folding, and `schema of` passes check, emits, and compiles clean under
//! the real Gleam toolchain. The generated project lives under the
//! workspace `target/` directory (never the system temp dir).

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::{check_in, emit_in, parse};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    crate_root()
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to resolve repository root from CARGO_MANIFEST_DIR".into())
}

/// The B1 landing bar: the ergonomics fixture checks, emits, and `gleam
/// build`s clean (#248 provenance protocol applies under parallel load).
#[test]
fn flow_vocab_b1_fixture_compiles_under_gleam() -> Result<(), Box<dyn Error>> {
    let fixture = crate_root().join("tests/fixtures/rev2/ergonomics/valid/flow_vocab_b1.awl");
    let source = fs::read_to_string(&fixture)?;
    let document = parse(&source)?;
    let fixture_dir = fixture
        .parent()
        .ok_or("fixture path has no parent directory")?;
    let errors = check_in(&document, fixture_dir);
    assert!(errors.is_empty(), "fixture must check clean: {errors:?}");
    let generated = emit_in(&document, fixture_dir)?;

    let repo_root = repo_root()?;
    let project = repo_root.join("target/flow-vocab-b1-proof");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_flow_vocab_b1_proof\"\nversion = \"0.1.0\"\ntarget = \
             \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \
             \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            repo_root.join("gleam/aion_flow").display()
        ),
    )?;
    fs::write(project.join("src/flow_vocab_b1.gleam"), generated)?;

    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&project)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("gleam binary is required for AWL emitter compile-proof tests: {error}"),
            )
        })?;
    assert!(
        output.status.success(),
        "gleam build failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
