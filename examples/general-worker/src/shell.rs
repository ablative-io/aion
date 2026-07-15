//! Typed process boundary used by `run_command` and hermetic shim tests.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// A completed child process, including nonzero exits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliRun {
    /// Process exit code.
    pub exit_code: i32,
    /// Captured stdout only.
    pub stdout: String,
    /// Captured stdout followed by stderr.
    pub output: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// A command that could not be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliFailure {
    /// The executable did not resolve on the effective `PATH`.
    ExecutableNotFound {
        /// Unresolved executable name.
        executable: String,
    },
    /// The process could not be spawned.
    SpawnFailed {
        /// Operating-system or boundary validation diagnostic.
        reason: String,
    },
}

impl CliFailure {
    /// Render a stable terminal diagnostic.
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

/// Process runner with an injectable executable search path.
#[derive(Clone, Debug)]
pub struct Shell {
    path_override: Option<OsString>,
}

impl Shell {
    /// Use the parent process's `PATH`.
    #[must_use]
    pub const fn inherited() -> Self {
        Self {
            path_override: None,
        }
    }

    /// Use exactly `path` for executable resolution and the child's `PATH`.
    pub fn with_path(path: impl Into<OsString>) -> Self {
        Self {
            path_override: Some(path.into()),
        }
    }

    /// Run one executable in `cwd` and capture its complete outcome.
    ///
    /// # Errors
    ///
    /// Returns [`CliFailure::SpawnFailed`] for a dead working directory or
    /// operating-system spawn error and [`CliFailure::ExecutableNotFound`] when
    /// the executable is absent from the effective `PATH`. Nonzero exits are
    /// successful boundary calls represented by [`CliRun::exit_code`].
    pub fn run(&self, executable: &str, args: &[String], cwd: &str) -> Result<CliRun, CliFailure> {
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
        let output = command.output().map_err(|source| CliFailure::SpawnFailed {
            reason: source.to_string(),
        })?;
        let elapsed = started.elapsed().as_millis();
        let duration_ms = u64::try_from(elapsed).map_or(u64::MAX, |value| value);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut combined = stdout.clone();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        Ok(CliRun {
            exit_code: exit_code(output.status),
            stdout,
            output: combined,
            duration_ms,
        })
    }

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
    match status.code() {
        Some(code) => code,
        None => -1,
    }
}

#[cfg(unix)]
fn is_executable_file(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    match candidate.metadata() {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(candidate: &Path) -> bool {
    candidate.is_file()
}
