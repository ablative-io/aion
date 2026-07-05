//! Activity handler bodies for the three brief-forge agent rounds,
//! generalized from stacked-dev's norn-backed handlers (`scout`, `dev`,
//! `dev_review` in `examples/stacked-dev/norn-worker/src/handlers.rs`).
//!
//! Each handler shells the `norn` CLI headlessly through
//! [`crate::shell::Shell`]: `--print`, the deterministic `--session-id` from
//! the activity input (`--resume-if-exists`, so the design session keeps its
//! own context across refute-loop rounds), `--workspace-root` confining file
//! tools to the target repo, `--output-schema` carrying the stage contract
//! inline, `--output-format json` for the envelope, and — the light-mode
//! pilot policy — `--model gpt-5.5` for all three stages. Reasoning efforts
//! follow the doctrine profiles (scout medium; designer and refuter high).
//!
//! Failure classification follows stacked-dev: a CLI that cannot run and a
//! command the contract requires to exit zero are **terminal** activity
//! failures; unparseable norn output is terminal carrying the head of the
//! output. The functions are plain synchronous `(Shell, Input) -> Result` so
//! hermetic tests can drive them with fake-CLI shims on a private `PATH`;
//! `main.rs` adapts them onto the worker's async handler signature.

use aion_worker::ActivityFailure;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::schemas::{BRIEF_OUTPUT_SCHEMA, REFUTATION_OUTPUT_SCHEMA, SCOUT_OUTPUT_SCHEMA};
use crate::shell::{CliRun, Shell};
use crate::types::{AgentRound, Brief, Refutation, ScoutReport};

/// How much of an unparseable norn stdout rides in the terminal failure
/// message. Presentational truncation only: enough to diagnose the envelope
/// shape without shipping megabytes of agent transcript through the failure
/// payload.
const UNPARSEABLE_OUTPUT_HEAD: usize = 1000;

/// The model every stage runs on — the light-mode pilot policy pins all
/// three agent rounds to one cheap frontier model.
const PILOT_MODEL: &str = "gpt-5.5";

/// `scout`: the grounding recon round — replace descriptions of the code
/// with observations of it. Output is a scout report
/// (`schemas/scout-report.schema.json`).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither the bare report nor the
/// `{"output": …}` envelope.
pub fn scout(shell: &Shell, round: AgentRound) -> Result<ScoutReport, ActivityFailure> {
    let command_run = run_norn(shell, round, "medium", SCOUT_OUTPUT_SCHEMA, "norn scout")?;
    parse_report::<ScoutReport>(&command_run, "norn scout")
}

/// `design`: the brief-drafting round — root cause, fix design, and
/// outcome-asserting gates. Output is a brief (`schemas/brief.schema.json`).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither documented shape.
pub fn design(shell: &Shell, round: AgentRound) -> Result<Brief, ActivityFailure> {
    let command_run = run_norn(shell, round, "high", BRIEF_OUTPUT_SCHEMA, "norn design")?;
    parse_report::<Brief>(&command_run, "norn design")
}

/// `refute`: the attack round — the refuter sees the brief artifact and the
/// scout report, never the designer's reasoning (the workflow's prompt
/// projection enforces that; this handler only relays). Output is a
/// refutation (`schemas/refutation.schema.json`).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither documented shape.
pub fn refute(shell: &Shell, round: AgentRound) -> Result<Refutation, ActivityFailure> {
    let command_run = run_norn(
        shell,
        round,
        "high",
        REFUTATION_OUTPUT_SCHEMA,
        "norn refute",
    )?;
    parse_report::<Refutation>(&command_run, "norn refute")
}

/// One headless norn invocation in the round's repo root: the shared flag
/// set of stacked-dev's norn handlers plus the pilot `--model` pin.
fn run_norn(
    shell: &Shell,
    round: AgentRound,
    reasoning_effort: &str,
    output_schema: &str,
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    let AgentRound {
        repo_root,
        session_id,
        prompt,
    } = round;
    require_run(
        shell,
        "norn",
        &[
            "--print",
            "--fast",
            "--model",
            PILOT_MODEL,
            "--reasoning-effort",
            reasoning_effort,
            "--session-id",
            &session_id,
            "--resume-if-exists",
            "--workspace-root",
            &repo_root,
            "--output-schema",
            output_schema,
            "--output-format",
            "json",
            &prompt,
        ],
        &repo_root,
        context,
    )
}

// --- helpers (stacked-dev norn-worker, verbatim) ------------------------------

/// Require a command to run AND exit zero; anything else is a terminal
/// activity failure carrying the command's diagnostics.
fn require_run(
    shell: &Shell,
    executable: &str,
    args: &[&str],
    cwd: &str,
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    match shell.run(executable, args, cwd) {
        Ok(command_run) if command_run.succeeded() => Ok(command_run),
        Ok(command_run) => Err(ActivityFailure::terminal(format!(
            "{context} failed — exit status {}: {}",
            command_run.exit_status,
            command_run.output.trim()
        ))),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "{context}: {}",
            failure.message()
        ))),
    }
}

/// Decode a norn command's stdout as a stage report, generic over the report
/// type.
///
/// CONFIRMED against real norn (stacked-dev live run, 2026-06-13):
/// `--output-format json` emits a completion envelope with the
/// schema-constrained result under `"output"`, alongside
/// `usage`/`model`/`session_id`/`events` (ignored here). Exactly two shapes
/// are attempted — the bare report (fake-CLI shims emit it raw), then norn's
/// `{"output": <report>}` envelope — plus the first JSON-looking line as a
/// last resort; if ALL fail the activity fails terminally carrying the head
/// of the output. No silent fallback beyond that documented attempt.
fn parse_report<T: DeserializeOwned>(
    command_run: &CliRun,
    context: &str,
) -> Result<T, ActivityFailure> {
    #[derive(Deserialize)]
    struct NornEnvelope<T> {
        output: T,
    }

    let trimmed = command_run.stdout.trim();
    if let Ok(report) = serde_json::from_str::<T>(trimmed) {
        return Ok(report);
    }
    if let Ok(envelope) = serde_json::from_str::<NornEnvelope<T>>(trimmed) {
        return Ok(envelope.output);
    }

    // Try extracting the first JSON line from stdout as a last resort.
    if let Some(json_line) = first_json_line(trimmed) {
        if let Ok(report) = serde_json::from_str::<T>(json_line) {
            return Ok(report);
        }
        if let Ok(envelope) = serde_json::from_str::<NornEnvelope<T>>(json_line) {
            return Ok(envelope.output);
        }
    }

    Err(ActivityFailure::terminal(format!(
        "{context} produced unparseable output \
         (tried the bare report shape and norn's {{\"output\": …}} envelope): {}",
        head(trimmed, UNPARSEABLE_OUTPUT_HEAD)
    )))
}

/// Extract the first line that looks like a complete JSON object or array.
fn first_json_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| {
        (line.starts_with('{') && line.ends_with('}'))
            || (line.starts_with('[') && line.ends_with(']'))
    })
}

/// First `limit` characters of `text`, truncated on a char boundary.
fn head(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((boundary, _)) => &text[..boundary],
        None => text,
    }
}
