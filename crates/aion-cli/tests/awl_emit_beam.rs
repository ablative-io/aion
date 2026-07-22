//! Live-binary proof for `aion awl emit --target beam`: the real `aion` binary
//! emits BEAM bytes byte-identical to the entry module inside
//! `compile_and_assemble_awl`'s archive for the same source — the ops-console
//! compatibility guarantee, proven end to end through the shipped binary, not
//! just the in-process seam. Also pins the stdout refusal when `--output` is
//! omitted (binary bytes never go to stdout).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aion_package::{ExtractionLimits, Package};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The committed hello fixture (identical to `examples/awl-hello/awl_hello.awl`).
fn hello_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aion-awl/tests/fixtures/rev2/flagship/valid/awl_hello.awl")
}

/// Invokes the built `aion` binary with `args`, capturing its output.
fn run_cli(args: &[&str]) -> Result<Output, std::io::Error> {
    Command::new(env!("CARGO_BIN_EXE_aion")).args(args).output()
}

/// Invokes the built `aion` binary with `args` from working directory `cwd`,
/// capturing its output — lets a test prove `schema("…")` resolution ignores
/// the process cwd and follows the document's own directory.
fn run_cli_in(cwd: &Path, args: &[&str]) -> Result<Output, std::io::Error> {
    Command::new(env!("CARGO_BIN_EXE_aion"))
        .current_dir(cwd)
        .args(args)
        .output()
}

/// A committed AWL document with a RELATIVE `schema("ticket.schema.json")`
/// import resolved beside it — the staged-import root evidence for Deliverable
/// A.2.
fn schema_import_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/schema_import/import_ticket.awl")
}

/// The live gate-6 proof: build + run the binary, then byte-compare its output
/// against the archive-internal entry module bytes for the same source.
#[test]
fn emit_beam_binary_output_equals_the_archive_entry_module() -> TestResult {
    let fixture = hello_fixture();
    let source = std::fs::read_to_string(&fixture)?;
    let schema_root = fixture
        .parent()
        .ok_or("hello fixture has no parent directory")?;

    let temp = tempfile::tempdir()?;
    let output = temp.path().join("awl_hello.beam");

    let run = run_cli(&[
        "awl",
        "emit",
        "--target",
        "beam",
        fixture.to_string_lossy().as_ref(),
        "-o",
        output.to_string_lossy().as_ref(),
    ])?;
    assert_eq!(
        run.status.code(),
        Some(0),
        "the binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let cli_bytes = std::fs::read(&output)?;
    assert!(
        cli_bytes.starts_with(b"FOR1"),
        "the emitted file is not a BEAM container"
    );

    // The archive the ops console deploys carries the same module bytes.
    let prepared = aion_awl_package::compile_and_assemble_awl(&source, schema_root)?;
    let package = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;
    let entry_module = package.manifest().entry_module.clone();
    let archive_bytes = package
        .beams()
        .get(&entry_module)
        .ok_or("archive lost its entry module")?;

    assert_eq!(
        cli_bytes.as_slice(),
        archive_bytes,
        "the binary's beam bytes drifted from the archive entry module"
    );
    Ok(())
}

/// Staged-import schema-root parity (Deliverable A.2), proven end to end: a
/// document with a RELATIVE `schema("ticket.schema.json")` import, emitted by
/// the binary run from an UNRELATED working directory, still resolves the schema
/// beside the document (not beside the cwd) and produces bytes byte-identical to
/// `compile_and_assemble_awl(source, document_parent)`. Were the CLI to root
/// `schema("…")` at the process cwd, this emit would fail to find the schema
/// from the temp cwd — so the parity is evidenced, not merely implemented (BC-5
/// review advisory A).
#[test]
fn emit_beam_resolves_schema_imports_from_the_document_directory() -> TestResult {
    let fixture = schema_import_fixture();
    let source = std::fs::read_to_string(&fixture)?;
    let document_parent = fixture
        .parent()
        .ok_or("schema-import fixture has no parent directory")?;

    // Run the binary from a temp cwd that is NOT the document's directory, so a
    // cwd-rooted schema resolution would fail to find `ticket.schema.json`.
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("import_ticket.beam");
    let run = run_cli_in(
        temp.path(),
        &[
            "awl",
            "emit",
            "--target",
            "beam",
            fixture.to_string_lossy().as_ref(),
            "-o",
            output.to_string_lossy().as_ref(),
        ],
    )?;
    assert_eq!(
        run.status.code(),
        Some(0),
        "the binary failed (schema import unresolved from an unrelated cwd?): {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let cli_bytes = std::fs::read(&output)?;

    let prepared = aion_awl_package::compile_and_assemble_awl(&source, document_parent)?;
    let package = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;
    let entry_module = package.manifest().entry_module.clone();
    let archive_bytes = package
        .beams()
        .get(&entry_module)
        .ok_or("archive lost its entry module")?;
    assert_eq!(
        cli_bytes.as_slice(),
        archive_bytes,
        "schema-import beam bytes drifted between the CLI and the archive seam"
    );
    Ok(())
}

/// The binary refuses `--target beam` without `--output`: BEAM bytes are never
/// written to stdout.
#[test]
fn emit_beam_binary_refuses_stdout() -> TestResult {
    let fixture = hello_fixture();
    let run = run_cli(&[
        "awl",
        "emit",
        "--target",
        "beam",
        fixture.to_string_lossy().as_ref(),
    ])?;
    assert_eq!(run.status.code(), Some(1), "expected a refusal exit code");
    assert!(run.stdout.is_empty(), "no bytes may reach stdout");
    let stderr = String::from_utf8(run.stderr)?;
    assert!(
        stderr.contains("requires `--output`"),
        "stderr was: {stderr}"
    );
    Ok(())
}
