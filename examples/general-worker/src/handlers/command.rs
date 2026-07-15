//! `run_command` handler.

use aion_worker::ActivityFailure;

use crate::clip::clip_text;
use crate::shell::Shell;
use crate::types::{CommandInput, CommandOutput};

/// Run an arbitrary command through the typed shell boundary.
///
/// # Errors
///
/// Returns a terminal failure when `argv` is empty or the process cannot be
/// started. A completed nonzero exit is returned as ordinary result data with
/// `passed: false`.
pub fn run_command(shell: &Shell, input: CommandInput) -> Result<CommandOutput, ActivityFailure> {
    let Some((executable, arguments)) = input.argv.split_first() else {
        return Err(ActivityFailure::terminal(
            "run_command input `argv` must contain an executable",
        ));
    };

    let run = shell
        .run(executable, arguments, &input.workspace_path)
        .map_err(|failure| {
            ActivityFailure::terminal(format!(
                "run_command `{}` could not run: {}",
                input.name,
                failure.message()
            ))
        })?;

    Ok(CommandOutput {
        name: input.name,
        argv: input.argv,
        exit_code: run.exit_code,
        passed: run.exit_code == 0,
        stdout: clip_text(&run.stdout),
        output: clip_text(&run.output),
        duration_ms: run.duration_ms,
    })
}
