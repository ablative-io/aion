//! Typed shell-out boundary for the staged-rounds shell activities (the
//! `dev-brief` worker's proven boundary, reused verbatim).
//!
//! Every command outcome is typed: a missing executable is loud, structured
//! data (never a silent skip), and a non-zero exit status is recorded data the
//! caller interprets (the cargo gate's pass/fail verdict, a merge conflict).
//! Output is the combined stdout/stderr text (stdout first); `stdout` alone is
//! also captured so a JSON parse never has to read a stderr tail.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// A completed command run: the exit status, the captured stdout, the combined
/// stdout/stderr text, and the wall-clock duration of the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliRun {
    /// The process exit status (`128 + signal` when killed by a signal, the
    /// shell convention).
    pub exit_status: i32,
    /// Captured stdout only. norn writes its JSON result to stdout and its
    /// WARN/INFO log lines to stderr, so a JSON parse MUST read this, never
    /// [`Self::output`] (whose stderr tail would corrupt the parse).
    pub stdout: String,
    /// Combined stdout/stderr text (stdout first) — for human diagnostics (the
    /// cargo gate, a failed-command error message), not for JSON parsing.
    pub output: String,
    /// Wall-clock duration of the process.
    pub duration_ms: u64,
}

impl CliRun {
    /// Whether the command exited with status zero.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_status == 0
    }
}

/// A command that could not run at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliFailure {
    /// The executable is not on `PATH`. With nothing shimmed this is a loud
    /// activity failure — activities are never silently skipped.
    ExecutableNotFound {
        /// The executable that could not be resolved.
        executable: String,
    },
    /// The process could not be started (for example, a missing working
    /// directory).
    SpawnFailed {
        /// Why the spawn failed.
        reason: String,
    },
}

impl CliFailure {
    /// Render the failure as a single diagnostic line, wording identical to
    /// `cli.failure_message` in the Gleam local implementations.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::ExecutableNotFound { executable } => {
                format!("executable not found on PATH: {executable}")
            }
            Self::SpawnFailed { reason } => {
                format!("command could not be spawned: {reason}")
            }
        }
    }
}

/// The process runner. Production constructs [`Shell::inherited`]; the
/// hermetic tests construct [`Shell::with_path`] pointing at a directory of
/// fake-CLI shims **alone**, so the handlers really shell out and the shims
/// intercept at the process boundary — the most realistic seam.
#[derive(Clone, Debug)]
pub struct Shell {
    path_override: Option<OsString>,
}

impl Shell {
    /// Resolve executables against the process's own `PATH`.
    #[must_use]
    pub fn inherited() -> Self {
        Self {
            path_override: None,
        }
    }

    /// Resolve executables against exactly this search path, and hand the
    /// same `PATH` to the child process.
    pub fn with_path(path: impl Into<OsString>) -> Self {
        Self {
            path_override: Some(path.into()),
        }
    }

    /// Run `executable` with `args` in `cwd`, capturing exit status,
    /// combined output, and duration.
    ///
    /// # Errors
    ///
    /// Fails with [`CliFailure::SpawnFailed`] when `cwd` is not a directory
    /// or the process cannot start, and [`CliFailure::ExecutableNotFound`]
    /// when `executable` does not resolve on the effective search path. A
    /// non-zero exit status is NOT an error — it is recorded data on the
    /// returned [`CliRun`].
    pub fn run(&self, executable: &str, args: &[&str], cwd: &str) -> Result<CliRun, CliFailure> {
        // A missing working directory would surface from spawn as the same
        // `NotFound` io error as a missing binary on some platforms; check it
        // first so the two failures stay distinguishable.
        if !Path::new(cwd).is_dir() {
            return Err(CliFailure::SpawnFailed {
                reason: format!("working directory does not exist: {cwd}"),
            });
        }
        let resolved =
            self.find_executable(executable)
                .ok_or_else(|| CliFailure::ExecutableNotFound {
                    executable: executable.to_owned(),
                })?;

        let mut command = Command::new(resolved);
        command.args(args).current_dir(cwd);
        if let Some(path) = &self.path_override {
            command.env("PATH", path);
        }

        let started = Instant::now();
        let output = command.output().map_err(|error| CliFailure::SpawnFailed {
            reason: error.to_string(),
        })?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let stdout_text = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut combined = stdout_text.clone();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        Ok(CliRun {
            exit_status: exit_code(output.status),
            stdout: stdout_text,
            output: combined,
            duration_ms,
        })
    }

    /// Resolve `executable` against the effective search path, mirroring
    /// `os:find_executable/1` in the Erlang port runner.
    fn find_executable(&self, executable: &str) -> Option<PathBuf> {
        let search_path = match &self.path_override {
            Some(path) => path.clone(),
            None => std::env::var_os("PATH")?,
        };
        std::env::split_paths(&search_path)
            .map(|directory| directory.join(executable))
            .find(|candidate| is_executable_file(candidate))
    }
}

/// Map an exit status to the integer the handlers interpret. A
/// signal-terminated process maps to `128 + signal` (the shell convention),
/// so it reads as a loud non-zero status rather than a fake success.
#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => -1,
    }
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

#[cfg(unix)]
fn is_executable_file(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    candidate
        .metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(candidate: &Path) -> bool {
    candidate.is_file()
}
