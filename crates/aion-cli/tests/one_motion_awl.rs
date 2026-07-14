//! No-server CLI regression tests for native one-motion AWL refusals.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const UNUSED_ENDPOINT: &str = "127.0.0.1:1";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aion-awl/tests/fixtures/rev2")
        .join(relative)
}

fn run_cli(args: &[&str]) -> Result<Output, std::io::Error> {
    Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["--endpoint", UNUSED_ENDPOINT])
        .args(args)
        .output()
}

#[test]
fn invalid_awl_deploy_exits_nonzero_with_the_compiler_span_diagnostic() -> TestResult {
    let invalid = fixture("declarations/invalid/call_unknown_action.awl");
    let output = run_cli(&["deploy", invalid.to_string_lossy().as_ref()])?;
    let stderr = String::from_utf8(output.stderr)?;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        stderr.contains("no action or child named"),
        "stderr was: {stderr}"
    );
    assert!(stderr.contains(" at line "), "stderr was: {stderr}");
    assert!(stderr.contains("column"), "stderr was: {stderr}");
    assert!(
        !stderr.contains("failed to connect"),
        "stderr was: {stderr}"
    );
    Ok(())
}

#[test]
fn mismatching_run_input_prints_schema_without_attempting_deploy() -> TestResult {
    let valid = fixture("flagship/valid/awl_hello.awl");
    let output = run_cli(&[
        "run",
        valid.to_string_lossy().as_ref(),
        "--input",
        r#"{"name":42}"#,
    ])?;
    let stderr = String::from_utf8(output.stderr)?;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        stderr.starts_with("input validation failed:"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("expected schema: {"),
        "stderr was: {stderr}"
    );
    assert!(stderr.contains("\"name\""), "stderr was: {stderr}");
    assert!(
        !stderr.contains("failed to connect"),
        "stderr was: {stderr}"
    );
    Ok(())
}
