//! Activity handler bodies for the dev-pipeline activities: the three
//! brief-forge agent rounds plus implement-and-gate's workspace, implementer,
//! and gate steps — generalized from stacked-dev's norn-backed handlers
//! (`scout`, `dev`, `dev_resume`, `provision_workspace`, `full_checks` in
//! `examples/stacked-dev/norn-worker/src/handlers.rs`).
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

use crate::schemas::{
    BRIEF_OUTPUT_SCHEMA, IMPLEMENTATION_REPORT_OUTPUT_SCHEMA, REFUTATION_OUTPUT_SCHEMA,
    SCOUT_OUTPUT_SCHEMA,
};
use crate::shell::{CliRun, Shell};
use crate::types::{
    AgentRound, Brief, GateCliRun, GateRun, ImplementRound, ImplementationReport, Isolation,
    ProvisionInput, Refutation, ScoutReport, TeardownInput, TornDown, Workspace,
};

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

// --- implement-and-gate --------------------------------------------------------

/// How much of a gate command's combined output rides in the recorded
/// [`GateCliRun`]. Tail-bounded (the END of the output — where compilers and
/// test runners put the verdict) so the durable record and the fix-round
/// prompt stay a sane size; the bound is the capture, so the workflow never
/// re-paraphrases.
const GATE_OUTPUT_TAIL: usize = 16_000;

/// Scratch directory (under the source repo root) holding provisioned
/// workspaces.
const WORKSPACES_DIR: &str = ".dev-pipeline-workspaces";

/// `provision_workspace`: create an isolated git worktree or clone of the
/// source repository at `base_ref`, named `dev-pipeline-<task_ref>` (with an
/// `-attempt-<N>` suffix when a previous run already holds the name).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `git` cannot run, when the scratch
/// directory cannot be created, or when provisioning fails for any reason
/// other than a name collision — nothing downstream can run without the
/// workspace.
pub fn provision_workspace(
    shell: &Shell,
    input: ProvisionInput,
) -> Result<Workspace, ActivityFailure> {
    let ProvisionInput {
        repo_root,
        base_ref,
        isolation,
        task_ref,
    } = input;
    let scratch_root = format!("{repo_root}/{WORKSPACES_DIR}");
    std::fs::create_dir_all(&scratch_root).map_err(|source| {
        ActivityFailure::terminal(format!(
            "provision_workspace: cannot create scratch directory {scratch_root}: {source}"
        ))
    })?;

    let base_name = format!("dev-pipeline-{task_ref}");
    match isolation {
        Isolation::Worktree => provision_worktree(shell, &repo_root, &base_ref, &base_name),
        Isolation::Clone => provision_clone(shell, &repo_root, &base_ref, &base_name),
    }
}

/// Provision via `git worktree add -b <name> <path> <base_ref>`, retrying
/// with `-attempt-<N>` suffixes only when git reports the branch/path
/// `already exists`; any other failure is terminal carrying git's output.
fn provision_worktree(
    shell: &Shell,
    repo_root: &str,
    base_ref: &str,
    base_name: &str,
) -> Result<Workspace, ActivityFailure> {
    for name in candidate_names(base_name) {
        let path = format!("{repo_root}/{WORKSPACES_DIR}/{name}");
        let run = match shell.run(
            "git",
            &["worktree", "add", "-b", &name, &path, base_ref],
            repo_root,
        ) {
            Ok(run) => run,
            Err(failure) => {
                return Err(ActivityFailure::terminal(format!(
                    "git worktree add: {}",
                    failure.message()
                )));
            }
        };
        if run.succeeded() {
            return Ok(Workspace { path });
        }
        if !run.output.contains("already exists") {
            return Err(ActivityFailure::terminal(format!(
                "git worktree add failed — exit status {}: {}",
                run.exit_status,
                run.output.trim()
            )));
        }
    }
    Err(name_space_exhausted(base_name))
}

/// Provision via `git clone <repo_root> <path>` followed by a checkout of
/// `base_ref` inside the clone. Name collisions are resolved by path
/// existence (a clone into an existing directory is the collision).
fn provision_clone(
    shell: &Shell,
    repo_root: &str,
    base_ref: &str,
    base_name: &str,
) -> Result<Workspace, ActivityFailure> {
    for name in candidate_names(base_name) {
        let path = format!("{repo_root}/{WORKSPACES_DIR}/{name}");
        if std::path::Path::new(&path).exists() {
            continue;
        }
        require_run(
            shell,
            "git",
            &["clone", repo_root, &path],
            repo_root,
            "git clone",
        )?;
        require_run(
            shell,
            "git",
            &["checkout", base_ref],
            &path,
            "git checkout (clone base_ref)",
        )?;
        return Ok(Workspace { path });
    }
    Err(name_space_exhausted(base_name))
}

/// The deterministic candidate sequence: the base name, then
/// `-attempt-2`..`-attempt-1000` (the stacked-dev provisioning convention).
fn candidate_names(base_name: &str) -> impl Iterator<Item = String> + '_ {
    std::iter::once(base_name.to_owned())
        .chain((2..=1000).map(move |attempt| format!("{base_name}-attempt-{attempt}")))
}

fn name_space_exhausted(base_name: &str) -> ActivityFailure {
    ActivityFailure::terminal(format!(
        "could not find an unused workspace name after 1000 attempts (base: {base_name})"
    ))
}

/// `implement`: the implementer round — norn run INSIDE the workspace (the
/// stacked-dev dev-handler pattern), in the deterministic
/// `<task_ref>-implement` session. Output is an implementation report
/// (`schemas/implementation-report.schema.json`).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither documented shape.
pub fn implement(
    shell: &Shell,
    round: ImplementRound,
) -> Result<ImplementationReport, ActivityFailure> {
    run_implementer(shell, round, "norn implement")
}

/// `implement_resume`: resume the SAME implementer session with a failing
/// gate's captured output as the feedback prompt (same invocation shape as
/// `implement` — `--resume-if-exists` on the deterministic session id is the
/// resume). Returns a FULL replacement implementation report.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither documented shape.
pub fn implement_resume(
    shell: &Shell,
    round: ImplementRound,
) -> Result<ImplementationReport, ActivityFailure> {
    run_implementer(shell, round, "norn implement resume")
}

/// One headless implementer invocation INSIDE the workspace: the brief-forge
/// flag set with the workspace as both cwd and `--workspace-root`, the
/// profile's `medium` reasoning effort, and the input's model override when
/// the workflow's frontier escape hatch set one (else the pilot model).
fn run_implementer(
    shell: &Shell,
    round: ImplementRound,
    context: &str,
) -> Result<ImplementationReport, ActivityFailure> {
    let ImplementRound {
        workspace_path,
        session_id,
        prompt,
        model,
    } = round;
    let model = model.as_deref().unwrap_or(PILOT_MODEL);
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--fast",
            "--model",
            model,
            "--reasoning-effort",
            "medium",
            "--session-id",
            &session_id,
            "--resume-if-exists",
            "--workspace-root",
            &workspace_path,
            "--output-schema",
            IMPLEMENTATION_REPORT_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &prompt,
        ],
        &workspace_path,
        context,
    )?;
    parse_report::<ImplementationReport>(&command_run, context)
}

/// `run_gate`: shell the exact gate command in the workspace via `sh -c` and
/// record `{exit_status, output, duration_ms}`. `sh`'s exit status IS the
/// command's own exit status — nothing sits between the command and the
/// verdict. A NON-ZERO EXIT IS DATA the workflow routes to the fix loop,
/// never an error here.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] only when the command cannot run at all — a
/// missing `sh` or a dead workspace directory is a broken environment, not a
/// red gate.
pub fn run_gate(shell: &Shell, gate: GateRun) -> Result<GateCliRun, ActivityFailure> {
    let GateRun {
        workspace_path,
        gate_id,
        command,
    } = gate;
    match shell.run("sh", &["-c", &command], &workspace_path) {
        Ok(command_run) => Ok(GateCliRun {
            exit_status: command_run.exit_status,
            output: tail(&command_run.output, GATE_OUTPUT_TAIL).to_owned(),
            duration_ms: command_run.duration_ms,
        }),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "gate {gate_id}: {}",
            failure.message()
        ))),
    }
}

/// `teardown_workspace`: best-effort workspace reclamation (a declared seam;
/// the `implement_and_gate` workflow deliberately never dispatches it — both
/// termini preserve the workspace for inspection). Tries `git worktree
/// remove --force` from the source repo, then plain directory removal for
/// clones.
///
/// # Errors
///
/// Never fails terminally — teardown is best-effort cleanup; the receipt
/// records whether the directory is actually gone.
pub fn teardown_workspace(
    shell: &Shell,
    input: TeardownInput,
) -> Result<TornDown, ActivityFailure> {
    let TeardownInput {
        repo_root,
        workspace_path,
    } = input;
    let _ = shell.run(
        "git",
        &["worktree", "remove", "--force", &workspace_path],
        &repo_root,
    );
    if std::path::Path::new(&workspace_path).exists() {
        let _ = std::fs::remove_dir_all(&workspace_path);
    }
    Ok(TornDown {
        cleaned: !std::path::Path::new(&workspace_path).exists(),
    })
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

/// Last `limit` characters of `text`, truncated on a char boundary — gate
/// output keeps its END, where compilers and test runners put the verdict.
fn tail(text: &str, limit: usize) -> &str {
    let char_count = text.chars().count();
    if char_count <= limit {
        return text;
    }
    match text.char_indices().nth(char_count - limit) {
        Some((boundary, _)) => &text[boundary..],
        None => text,
    }
}
