//// Typed shell-out boundary for the activity local implementations.
////
//// Local implementations run only under the `aion/testing` harness; deployed
//// runs dispatch the same activity names to Meridian workers. Every command
//// outcome is typed — a missing executable is loud, structured data, never a
//// silent skip, and a non-zero exit status is recorded data the caller
//// interprets (diagnostics for check runners, a forfeited cache for the
//// warm build).

import gleam/int
import gleam/string

/// A completed command run: the exit status, combined stdout/stderr text,
/// and the wall-clock duration of the process.
pub type CliRun {
  CliRun(exit_status: Int, output: String, duration_ms: Int)
}

/// A command that could not run at all.
pub type CliFailure {
  /// The executable is not on `PATH`. With no fake-CLI shim installed this
  /// is a loud activity failure — activities are never silently skipped.
  ExecutableNotFound(executable: String)
  /// The process could not be started (for example, a missing working
  /// directory).
  SpawnFailed(reason: String)
}

@external(erlang, "{{name}}_cli_ffi", "run_command")
fn raw_run_command(
  executable: String,
  args: List(String),
  cwd: String,
) -> Result(#(Int, String, Int), String)

/// Run `executable` with `args` in `cwd`, capturing exit status, output, and
/// duration.
pub fn run(
  executable: String,
  args: List(String),
  cwd: String,
) -> Result(CliRun, CliFailure) {
  case raw_run_command(executable, args, cwd) {
    Ok(#(exit_status, output, duration_ms)) ->
      Ok(CliRun(
        exit_status: exit_status,
        output: output,
        duration_ms: duration_ms,
      ))
    Error(raw_failure) -> Error(parse_failure(raw_failure))
  }
}

/// Whether the command exited with status zero.
pub fn succeeded(command_run: CliRun) -> Bool {
  command_run.exit_status == 0
}

/// Render a completed-but-failed run as a single diagnostic line.
pub fn run_diagnostics(command_run: CliRun) -> String {
  "exit status "
  <> int.to_string(command_run.exit_status)
  <> ": "
  <> string.trim(command_run.output)
}

/// Render a `CliFailure` as a single diagnostic line for typed errors.
pub fn failure_message(failure: CliFailure) -> String {
  case failure {
    ExecutableNotFound(executable: executable) ->
      "executable not found on PATH: " <> executable
    SpawnFailed(reason: reason) -> "command could not be spawned: " <> reason
  }
}

fn parse_failure(raw_failure: String) -> CliFailure {
  case raw_failure {
    "not_found:" <> executable -> ExecutableNotFound(executable: executable)
    "spawn:" <> reason -> SpawnFailed(reason: reason)
    _ -> SpawnFailed(reason: raw_failure)
  }
}
