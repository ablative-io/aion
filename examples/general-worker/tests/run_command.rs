#![cfg(unix)]

//! Hermetic process-boundary tests for `run_command`.

use std::error::Error;
use std::path::Path;

use aion_worker::{ActivityFailure, Classification};
use general_worker::clip::CLIP_LIMIT_CHARS;
use general_worker::{CommandInput, Shell, run_command};
use serde_json::json;

type TestResult = Result<(), Box<dyn Error>>;

struct Shims {
    directory: tempfile::TempDir,
}

impl Shims {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-temp");
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            directory: tempfile::Builder::new()
                .prefix("run-command-")
                .tempdir_in(root)?,
        })
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn root_string(&self) -> String {
        self.root().to_string_lossy().into_owned()
    }

    fn shell(&self) -> Shell {
        Shell::with_path(self.root())
    }

    fn write(&self, name: &str, body: &str) -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let path = self.root().join(name);
        let script = format!("#!/bin/sh\n{body}\n");
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn write_missing_interpreter_executable(&self, name: &str) -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let path = self.root().join(name);
        std::fs::write(&path, "#!/definitely/not/a/real/interpreter\n")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }
}

fn input(shims: &Shims, name: &str, argv: &[&str]) -> CommandInput {
    CommandInput {
        workspace_path: shims.root_string(),
        name: name.to_owned(),
        argv: argv.iter().map(|value| (*value).to_owned()).collect(),
    }
}

fn assert_terminal(failure: &ActivityFailure, expected: &str) {
    assert_eq!(failure.classification(), &Classification::Terminal);
    assert!(
        failure.message().contains(expected),
        "message {:?} must contain {expected:?}",
        failure.message()
    );
}

#[test]
fn success_preserves_contract_and_separates_stdout_from_stderr() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "tool",
        "pwd; /bin/sleep 0.05; printf 'stdout:%s\\n' \"$1\"; printf 'stderr-only\\n' >&2; exit 0",
    )?;

    let result = run_command(&shims.shell(), input(&shims, "probe", &["tool", "value"]))
        .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(result.name, "probe");
    assert_eq!(result.argv, vec!["tool", "value"]);
    assert_eq!(result.exit_code, 0);
    assert!(result.passed);
    let reported_cwd = result
        .stdout
        .lines()
        .next()
        .ok_or("successful command did not print its working directory")?;
    assert_eq!(
        std::fs::canonicalize(reported_cwd)?,
        std::fs::canonicalize(shims.root())?
    );
    let expected_stdout = format!("{reported_cwd}\nstdout:value\n");
    let expected_output = format!("{expected_stdout}stderr-only\n");
    assert_eq!(result.stdout, expected_stdout);
    assert_eq!(result.output, expected_output);
    assert!(
        result.duration_ms >= 25,
        "50 ms shim delay must be reflected in duration_ms, got {}",
        result.duration_ms
    );

    let wire = serde_json::to_value(&result)?;
    assert_eq!(
        wire,
        json!({
            "name": "probe",
            "argv": ["tool", "value"],
            "exit_code": 0,
            "passed": true,
            "stdout": result.stdout,
            "output": result.output,
            "duration_ms": result.duration_ms,
        })
    );
    assert_eq!(
        wire.as_object()
            .ok_or("serialized CommandOutput must be a JSON object")?
            .len(),
        7
    );
    Ok(())
}

#[test]
fn nonzero_exit_is_successful_result_data() -> TestResult {
    let shims = Shims::new()?;
    shims.write("check", "printf 'failed check\\n'; exit 23")?;

    let result = run_command(&shims.shell(), input(&shims, "check", &["check"]))
        .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(result.exit_code, 23);
    assert!(!result.passed);
    assert_eq!(result.stdout, "failed check\n");
    Ok(())
}

#[test]
fn empty_argv_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    let failure = run_command(&shims.shell(), input(&shims, "empty", &[]))
        .err()
        .ok_or("empty argv must fail")?;
    assert_terminal(&failure, "`argv` must contain an executable");
    Ok(())
}

#[test]
fn missing_executable_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    let failure = run_command(&shims.shell(), input(&shims, "missing", &["not-installed"]))
        .err()
        .ok_or("missing executable must fail")?;
    assert_terminal(&failure, "executable not found on PATH: not-installed");
    Ok(())
}

#[test]
fn missing_working_directory_is_terminal_before_spawn() -> TestResult {
    let shims = Shims::new()?;
    shims.write("tool", "exit 0")?;
    let mut command = input(&shims, "dead-cwd", &["tool"]);
    command.workspace_path = shims.root().join("absent").to_string_lossy().into_owned();

    let failure = run_command(&shims.shell(), command)
        .err()
        .ok_or("missing working directory must fail")?;
    assert_terminal(&failure, "working directory does not exist");
    Ok(())
}

#[test]
fn operating_system_spawn_failure_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    shims.write_missing_interpreter_executable("broken")?;

    let failure = run_command(&shims.shell(), input(&shims, "broken", &["broken"]))
        .err()
        .ok_or("an executable with a missing interpreter must fail to spawn")?;
    assert_terminal(&failure, "command could not be spawned");
    Ok(())
}

#[test]
fn stdout_and_combined_output_are_clipped_independently() -> TestResult {
    let shims = Shims::new()?;
    let count = CLIP_LIMIT_CHARS + 200;
    shims.write(
        "large",
        &format!(
            "i=0; while [ \"$i\" -lt {count} ]; do printf x; i=$((i + 1)); done; printf 'stderr-tail' >&2; exit 0"
        ),
    )?;

    let result = run_command(&shims.shell(), input(&shims, "large", &["large"]))
        .map_err(|failure| failure.message().to_owned())?;

    assert!(result.stdout.contains("--- output truncated:"));
    assert!(result.output.contains("--- output truncated:"));
    assert!(!result.stdout.contains("stderr-tail"));
    assert!(result.output.ends_with("stderr-tail"));
    Ok(())
}

#[test]
fn stderr_only_overflow_does_not_modify_short_stdout() -> TestResult {
    let shims = Shims::new()?;
    let count = CLIP_LIMIT_CHARS + 200;
    shims.write(
        "stderr-heavy",
        &format!(
            "printf 'short\\n'; i=0; while [ \"$i\" -lt {count} ]; do printf e >&2; i=$((i + 1)); done; exit 0"
        ),
    )?;

    let result = run_command(
        &shims.shell(),
        input(&shims, "stderr-heavy", &["stderr-heavy"]),
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(result.stdout, "short\n");
    assert!(!result.stdout.contains("truncated"));
    assert!(result.output.contains("--- output truncated:"));
    Ok(())
}
