//! End-to-end tests for the local `package` subcommand.
//!
//! Every invocation passes an unroutable `--endpoint`, so a zero exit code or
//! a packaging-shaped error is also proof that `package` never dials the
//! server (the connection would fail loudly if it were attempted).

use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// An endpoint no test environment can connect to; any attempt to dial it
/// fails immediately with a resolution or connection error.
const UNROUTABLE_ENDPOINT: &str = "aion-cli-package-test.invalid:1";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run_cli(args: &[&str], clear_path: bool) -> Result<Output, Box<dyn std::error::Error>> {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_aion-cli"));
    command.args(["--endpoint", UNROUTABLE_ENDPOINT]).args(args);
    if clear_path {
        command.env("PATH", "");
    }
    Ok(command.output()?)
}

fn temp_dir(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("aion-cli-package-{label}-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Runtime gate: the hello-world example must be built (`gleam build`).
fn built_hello_world() -> Option<PathBuf> {
    let project = repo_root().join("examples/hello-world");
    project.join("build/dev/erlang").is_dir().then_some(project)
}

#[test]
fn package_hello_world_succeeds_offline_with_result_document() -> TestResult {
    let Some(project) = built_hello_world() else {
        println!("skipping: examples/hello-world is not built; run `gleam build` there first");
        return Ok(());
    };
    let out_dir = temp_dir("hello")?;
    let out_path = out_dir.join("hello.aion");

    let output = run_cli(
        &[
            "package",
            project.to_str().ok_or("non-UTF-8 repo path")?,
            "--out",
            out_path.to_str().ok_or("non-UTF-8 temp path")?,
        ],
        false,
    )?;

    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        output.status.success(),
        "package failed: stdout={stdout} stderr={stderr}"
    );

    let document: serde_json::Value = serde_json::from_str(&stdout)?;
    let packages = document["packages"]
        .as_array()
        .ok_or("packages is not an array")?;
    assert_eq!(packages.len(), 1);
    let entry = &packages[0];
    assert_eq!(entry["workflow_type"], "hello_world");
    let version = entry["version"].as_str().ok_or("version not a string")?;
    assert_eq!(version.len(), 64);
    assert!(version.chars().all(|hex| hex.is_ascii_hexdigit()));
    assert_eq!(
        entry["deployed_name"].as_str(),
        Some(format!("hello_world${version}").as_str())
    );
    assert_eq!(
        entry["output"].as_str().map(Path::new),
        Some(out_path.as_path())
    );
    assert!(entry["modules"].as_u64().ok_or("modules not a count")? > 0);

    let excluded = document["excluded"]
        .as_array()
        .ok_or("excluded is not an array")?;
    assert!(excluded.iter().any(|entry| {
        entry["module"] == "aion_flow_ffi"
            && entry["package"] == "aion_flow"
            && entry["reason"] == "sdk_test_only"
    }));

    assert!(out_path.is_file());
    fs::remove_dir_all(&out_dir)?;
    Ok(())
}

/// `--out` is the caller's path and is exempt from the root confinement that
/// applies to `workflow.toml`-declared paths: a `..` traversal (here landing
/// outside any project root) must still be honoured.
#[test]
fn package_out_override_with_dotdot_is_exempt_from_confinement() -> TestResult {
    let Some(project) = built_hello_world() else {
        println!("skipping: examples/hello-world is not built; run `gleam build` there first");
        return Ok(());
    };
    let out_dir = temp_dir("hello-dotdot")?;
    fs::create_dir_all(out_dir.join("nested"))?;
    let out_arg = out_dir.join("nested/../hello-dotdot.aion");

    let output = run_cli(
        &[
            "package",
            project.to_str().ok_or("non-UTF-8 repo path")?,
            "--out",
            out_arg.to_str().ok_or("non-UTF-8 temp path")?,
        ],
        false,
    )?;

    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        output.status.success(),
        "package with dotdot --out failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        out_dir.join("hello-dotdot.aion").is_file(),
        "archive was not written through the dotdot --out path"
    );
    fs::remove_dir_all(&out_dir)?;
    Ok(())
}

#[test]
fn package_without_descriptor_fails_with_error_chain_and_no_dial() -> TestResult {
    let dir = temp_dir("missing-descriptor")?;

    let output = run_cli(
        &["package", dir.to_str().ok_or("non-UTF-8 temp path")?],
        false,
    )?;

    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "error runs must not write stdout");
    assert!(
        stderr.starts_with("error: failed to package workflow project"),
        "missing error prefix: {stderr}"
    );
    assert!(
        stderr.contains("no workflow.toml found"),
        "error chain does not name the missing descriptor: {stderr}"
    );
    assert!(
        !stderr.contains("failed to connect"),
        "package must not attempt a server connection: {stderr}"
    );
    fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn package_build_failure_is_surfaced_before_packaging() -> TestResult {
    let dir = temp_dir("build-failure")?;

    let output = run_cli(
        &[
            "package",
            dir.to_str().ok_or("non-UTF-8 temp path")?,
            "--build",
        ],
        true,
    )?;

    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "error runs must not write stdout");
    assert!(
        stderr.contains("gleam build"),
        "error chain does not name the build step: {stderr}"
    );
    fs::remove_dir_all(&dir)?;
    Ok(())
}
