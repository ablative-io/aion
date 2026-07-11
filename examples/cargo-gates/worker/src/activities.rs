//! Typed implementations of the four Cargo gate activities.
//!
//! Non-zero command statuses and validation failures are returned as ordinary
//! `GateResult` data so the workflow can compute its verdict from reality.

use std::path::Path;
use std::process::{Command, ExitStatus};

use aion_worker::ActivityFailure;
use serde::{Deserialize, Serialize};

const OUTPUT_TAIL_LINES: usize = 40;

/// Shared input shape for every gate action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInput {
    /// Workspace directory in which Cargo should run.
    pub path: String,
}

/// One gate's bounded, factual result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    /// Stable human-readable gate name.
    pub gate: String,
    /// Real process exit code, or `-1` when no process could be spawned.
    pub exit_code: i32,
    /// True exactly when the process exited with code zero.
    pub passed: bool,
    /// At most the final 40 lines of combined stdout and stderr.
    pub output_tail: String,
}

#[derive(Clone, Copy)]
struct Gate {
    name: &'static str,
    args: &'static [&'static str],
}

const CHECK: Gate = Gate {
    name: "check",
    args: &["check", "--workspace"],
};
const CLIPPY: Gate = Gate {
    name: "clippy",
    args: &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ],
};
const TESTS: Gate = Gate {
    name: "tests",
    args: &["test", "--workspace"],
};
const FMT: Gate = Gate {
    name: "fmt",
    args: &["fmt", "--check"],
};

/// Run `cargo check --workspace`.
///
/// # Errors
///
/// The activity contract is fallible for the worker SDK, but command and input
/// failures are deliberately represented by a successful [`GateResult`].
pub fn run_check(input: GateInput) -> Result<GateResult, ActivityFailure> {
    let GateInput { path } = input;
    Ok(run_gate(CHECK, &path))
}

/// Run `cargo clippy --workspace --all-targets -- -D warnings`.
///
/// # Errors
///
/// See [`run_check`].
pub fn run_clippy(input: GateInput) -> Result<GateResult, ActivityFailure> {
    let GateInput { path } = input;
    Ok(run_gate(CLIPPY, &path))
}

/// Run `cargo test --workspace`.
///
/// # Errors
///
/// See [`run_check`].
pub fn run_tests(input: GateInput) -> Result<GateResult, ActivityFailure> {
    let GateInput { path } = input;
    Ok(run_gate(TESTS, &path))
}

/// Run the non-mutating `cargo fmt --check` gate.
///
/// # Errors
///
/// See [`run_check`].
pub fn run_fmt_check(input: GateInput) -> Result<GateResult, ActivityFailure> {
    let GateInput { path } = input;
    Ok(run_gate(FMT, &path))
}

fn run_gate(gate: Gate, workspace: &str) -> GateResult {
    if !Path::new(workspace).is_dir() {
        return refusal(
            gate,
            &format!("workspace path does not exist or is not a directory: {workspace}"),
        );
    }
    if !Path::new(workspace).join("Cargo.toml").is_file() {
        return refusal(
            gate,
            &format!("workspace contains no Cargo.toml: {workspace}"),
        );
    }

    match Command::new("cargo")
        .args(gate.args)
        .current_dir(workspace)
        .output()
    {
        Ok(output) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            let exit_code = exit_code(output.status);
            GateResult {
                gate: gate.name.to_owned(),
                exit_code,
                passed: exit_code == 0,
                output_tail: tail(&combined),
            }
        }
        Err(error) => {
            let message = format!("command could not be spawned: {error}");
            refusal(gate, &message)
        }
    }
}

fn refusal(gate: Gate, message: &str) -> GateResult {
    GateResult {
        gate: gate.name.to_owned(),
        exit_code: -1,
        passed: false,
        output_tail: tail(message),
    }
}

fn tail(output: &str) -> String {
    let mut lines = output
        .lines()
        .rev()
        .take(OUTPUT_TAIL_LINES)
        .collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}

#[cfg(unix)]
fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => -1,
    }
}

#[cfg(not(unix))]
fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{GateInput, run_check, tail};

    #[test]
    fn check_runs_a_real_green_cargo_fixture() -> anyhow::Result<()> {
        let fixture = tempfile::tempdir()?;
        fs::create_dir(fixture.path().join("src"))?;
        fs::write(
            fixture.path().join("Cargo.toml"),
            "[package]\nname = \"green_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn green() -> bool { true }\n",
        )?;

        let result = run_check(GateInput {
            path: fixture.path().to_string_lossy().into_owned(),
        })?;
        assert_eq!(result.gate, "check");
        assert_eq!(result.exit_code, 0, "{}", result.output_tail);
        assert!(result.passed);
        assert!(result.output_tail.lines().count() <= 40);
        Ok(())
    }

    #[test]
    fn nonexistent_workspace_is_a_typed_refusal() -> anyhow::Result<()> {
        let fixture = tempfile::tempdir()?;
        let missing = fixture.path().join("missing");
        let result = run_check(GateInput {
            path: missing.to_string_lossy().into_owned(),
        })?;

        assert_eq!(result.gate, "check");
        assert_eq!(result.exit_code, -1);
        assert!(!result.passed);
        assert!(result.output_tail.contains("does not exist"));
        Ok(())
    }

    #[test]
    fn output_tail_is_bounded_to_the_last_forty_lines() {
        let output = (1..=50)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let bounded = tail(&output);
        assert_eq!(bounded.lines().count(), 40);
        assert_eq!(bounded.lines().next(), Some("11"));
        assert_eq!(bounded.lines().last(), Some("50"));
    }
}
