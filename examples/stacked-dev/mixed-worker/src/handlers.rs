//! Activity handler bodies, mirroring `../src/stacked_dev/locals.gleam`
//! invocation for invocation.
//!
//! Each handler shells to the real CLI that owns the step (`yg` for worktree
//! provisioning, affected-module scoping, and diagnostics checks, `norn` for
//! the dev agent, `cargo` for the advisory warm build, `meridian` for review
//! requests and landing) through [`crate::shell::Shell`]. Failure
//! classification follows the local implementations: a CLI that cannot run
//! (missing executable, dead working directory) and a command the contract
//! requires to exit zero are **terminal** activity failures — retrying a
//! broken environment cannot help. A non-zero exit that the contract treats
//! as recorded data (a forfeited warm cache, check diagnostics, a gate
//! report) is a successful activity result, never an error.
//!
//! The functions are plain synchronous `(Shell, Input) -> Result<Output, _>`
//! so the hermetic tests drive them directly with fake-CLI shims on a
//! private `PATH`; `main.rs` adapts them onto the worker's async handler
//! signature.

use std::path::PathBuf;

use aion_worker::ActivityFailure;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::schemas::{DEV_OUTPUT_SCHEMA, REVIEW_OUTPUT_SCHEMA, SCOUT_OUTPUT_SCHEMA};
use crate::shell::{CliRun, Shell};
use crate::types::{
    AssembleInput, AssembledWave, BriefDocument, CheckResult, CheckVerdict, DevInput, DevReport,
    EnrichInput, GateInput, GateResult, GateScope, GateVerdict, Isolation, LandInput, Landed,
    ProvisionInput, ResumeInput, ReviewAck, ReviewInput, ReviewReport, ReviewRequest, ScopedInput,
    ScoutInput, ScoutReport, StartupResult, StartupTask, TeardownInput, TornDown, Workspace,
};

/// How much of an unparseable norn stdout rides in the terminal failure
/// message. Presentational truncation only: enough to diagnose the envelope
/// shape without shipping megabytes of agent transcript through the failure
/// payload.
const UNPARSEABLE_OUTPUT_HEAD: usize = 1000;

/// Config directory for headless Claude Code invocations. Isolates workflow
/// sessions from the user's personal ~/.claude — carries its own settings
/// (with the diagnostic-crush hook) and session history.
const CLAUDE_CONFIG_DIR: &str = "~/.claude-workflow";

/// Environment variables injected into every `claude` child process.
fn claude_envs() -> Vec<(&'static str, &'static str)> {
    vec![("CLAUDE_CONFIG_DIR", CLAUDE_CONFIG_DIR)]
}

/// `provision_workspace`: provision an isolated workspace via the `yg` CLI.
///
/// Only worktree isolation has an implementation; the other typed variants
/// fail loudly, exactly like the local implementation's seam.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the isolation mode has no
/// implementation, when `yg` cannot run, or when either `yg` verb exits
/// non-zero.
pub fn provision_workspace(
    shell: &Shell,
    input: ProvisionInput,
) -> Result<Workspace, ActivityFailure> {
    match input.isolation {
        Isolation::Worktree => provision_worktree(shell, input),
        Isolation::Copy | Isolation::Overlay | Isolation::Vm => {
            Err(ActivityFailure::terminal(format!(
                "isolation mode {} is a typed seam with no local implementation \
                 (TODO(meridian): exchange-VM dispatch)",
                input.isolation.wire_name()
            )))
        }
    }
}

fn provision_worktree(shell: &Shell, input: ProvisionInput) -> Result<Workspace, ActivityFailure> {
    let ProvisionInput {
        repo_root,
        brief_id,
        base_ref,
        placement,
        isolation,
    } = input;
    let base_branch = format!("stacked-dev-{brief_id}");
    let branch = resolve_branch_name(shell, &base_branch, &base_ref, &repo_root)?;
    let worktree_path = format!("{repo_root}/.yggdrasil-worktrees/{branch}");

    require_run(
        shell,
        "yg",
        &["branch", "provision", &branch, "--path", &worktree_path],
        &repo_root,
        "yg branch provision",
    )?;
    Ok(Workspace {
        path: worktree_path,
        branch,
        placement,
        isolation,
    })
}

/// Try `yg branch add` with the base name, then `-attempt-2`, `-attempt-3`,
/// etc. until one succeeds. Returns the branch name that was added.
fn resolve_branch_name(
    shell: &Shell,
    base: &str,
    base_ref: &str,
    repo_root: &str,
) -> Result<String, ActivityFailure> {
    if try_branch_add(shell, base, base_ref, repo_root) {
        return Ok(base.to_owned());
    }
    let mut attempt = 2;
    loop {
        let candidate = format!("{base}-attempt-{attempt}");
        if try_branch_add(shell, &candidate, base_ref, repo_root) {
            return Ok(candidate);
        }
        attempt += 1;
        if attempt > 1000 {
            return Err(ActivityFailure::terminal(format!(
                "could not find an unused branch name after 1000 attempts (base: {base})"
            )));
        }
    }
}

/// Attempt `yg branch add`; returns true on success, false if the branch
/// already exists.
fn try_branch_add(shell: &Shell, branch: &str, base_ref: &str, repo_root: &str) -> bool {
    matches!(
        shell.run("yg", &["branch", "add", branch, base_ref], repo_root),
        Ok(run) if run.succeeded()
    )
}

/// Serve one startup fan-out task: the advisory warm build or the dev round.
/// Registered for BOTH the `warm_build` and `dev` activity names — the two
/// activities flow through one homogeneous `workflow.all` fan-out, so they
/// share the tagged `StartupTask`/`StartupResult` envelope.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the owning CLI cannot run (`cargo` for
/// the warm build, `norn` for the dev round), when `norn` exits non-zero, or
/// when its output matches neither documented `DevResult` shape. A failed
/// `cargo build` is NOT an error — it is the recorded `ok: false` outcome.
pub fn startup_task(shell: &Shell, task: StartupTask) -> Result<StartupResult, ActivityFailure> {
    match task {
        StartupTask::WarmBuild { workspace } => warm_build(shell, &workspace),
        StartupTask::Dev { dev_input } => dev(shell, &dev_input),
    }
}

/// Warm the build cache with `cargo build` in the workspace.
///
/// Advisory by contract: a failed build forfeits the warm cache and is
/// recorded as `ok: false` — it must never fail the run. A missing `cargo`
/// executable is still a loud terminal failure: that is a broken
/// environment, not a forfeited cache.
fn warm_build(shell: &Shell, workspace: &Workspace) -> Result<StartupResult, ActivityFailure> {
    match shell.run("cargo", &["build"], &workspace.path) {
        Ok(command_run) => Ok(StartupResult::Warmed {
            build_warm: crate::types::BuildWarm {
                ok: command_run.succeeded(),
                duration_ms: command_run.duration_ms,
            },
        }),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "cargo build: {}",
            failure.message()
        ))),
    }
}

/// Run the dev agent against the projected dev prompt via Claude Code headless.
///
/// Claude Code has no --workspace-root flag, so the working directory is set
/// to the worktree path. The --name flag sets a deterministic session name
/// that `dev_resume` can later --resume by.
fn dev(shell: &Shell, input: &DevInput) -> Result<StartupResult, ActivityFailure> {
    let session_id = input.workspace.branch.clone();
    let envs = claude_envs();

    let command_run = require_run_with_envs(
        shell,
        "claude",
        &[
            "--print",
            "--dangerously-skip-permissions",
            "--setting-sources",
            "user",
            "--name",
            &session_id,
            "--json-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &input.prompt,
        ],
        &input.workspace.path,
        &envs,
        "claude dev",
    )?;
    let dev_report = parse_report::<DevReport>(&command_run, "claude dev")?;
    Ok(StartupResult::Developed { dev_report })
}

/// `dev_resume`: resume the same Claude Code session with feedback
/// (scoped-check diagnostics). Returns a FULL replacement dev report.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when Claude Code cannot run, exits non-zero,
/// or answers with output matching neither the bare report nor the
/// `{"output": …}` envelope.
pub fn dev_resume(shell: &Shell, input: ResumeInput) -> Result<DevReport, ActivityFailure> {
    let ResumeInput {
        session_id,
        feedback,
        workspace_path,
    } = input;
    let envs = claude_envs();
    let command_run = require_run_with_envs(
        shell,
        "claude",
        &[
            "--print",
            "--dangerously-skip-permissions",
            "--setting-sources",
            "user",
            "--resume",
            &session_id,
            "--json-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &feedback,
        ],
        &workspace_path,
        &envs,
        "claude resume",
    )?;
    parse_report::<DevReport>(&command_run, "claude resume")
}

/// `scout`: the read-only orientation round in its own deterministic norn
/// session (`<branch>-scout`, CN4).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither the bare report nor the `{"output":
/// …}` envelope.
pub fn scout(shell: &Shell, input: ScoutInput) -> Result<ScoutReport, ActivityFailure> {
    let ScoutInput { workspace, prompt } = input;
    let session_id = format!("{}-scout", workspace.branch);
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--reasoning-effort",
            "medium",
            "--session-id",
            &session_id,
            "--resume-if-exists",
            "--workspace-root",
            &workspace.path,
            "--output-schema",
            SCOUT_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &prompt,
        ],
        &workspace.path,
        "norn scout",
    )?;
    parse_report::<ScoutReport>(&command_run, "norn scout")
}

/// `dev_review`: the adversarial reviewer round in its own deterministic norn
/// session (`<branch>-review` — NEVER the dev session, CN4).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither the bare report nor the `{"output":
/// …}` envelope.
pub fn dev_review(shell: &Shell, input: ReviewInput) -> Result<ReviewReport, ActivityFailure> {
    let ReviewInput { workspace, prompt } = input;
    let session_id = format!("{}-review", workspace.branch);
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--reasoning-effort",
            "x-high",
            "--session-id",
            &session_id,
            "--resume-if-exists",
            "--workspace-root",
            &workspace.path,
            "--output-schema",
            REVIEW_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &prompt,
        ],
        &workspace.path,
        "norn review",
    )?;
    parse_report::<ReviewReport>(&command_run, "norn review")
}

/// `scoped_checks`: compute the affected package set from the dependency
/// graph, then run diagnostics limited to it. An empty affected set falls
/// back LOUDLY to a named workspace-wide scope; zero checks are never run
/// silently.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `yg` cannot run or when the
/// affected-set query exits non-zero. A failing diagnostics run is NOT an
/// error — it is the recorded fail verdict carrying the diagnostics.
pub fn scoped_checks(shell: &Shell, input: ScopedInput) -> Result<CheckResult, ActivityFailure> {
    let ScopedInput {
        workspace,
        files_touched,
    } = input;
    // Affected packages come from the dependency graph: one bare crate name
    // per line (direct-only = the crates that actually contain the changed
    // files; the gate runs broad).
    let mut affected_args = vec!["graph", "affected", "--plain", "--direct-only"];
    affected_args.extend(files_touched.iter().map(String::as_str));
    let affected_run = require_run(
        shell,
        "yg",
        &affected_args,
        &workspace.path,
        "yg graph affected",
    )?;
    let packages: Vec<String> = affected_run
        .output
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();

    if packages.is_empty() {
        // No affected packages — fall back LOUDLY to a named workspace-wide
        // scope. Wording identical to the local implementation.
        let scope = "workspace-wide fallback: affected scoping returned an empty set";
        check_with(
            shell,
            &["diagnostics", "check", "--workspace", "--format", "json"],
            &workspace,
            Vec::new(),
            scope,
        )
    } else {
        // One scoped diagnostics run over exactly the affected packages.
        let mut args = vec!["diagnostics", "check", "--format", "json"];
        for name in &packages {
            args.push("--package");
            args.push(name);
        }
        let scope = format!("affected: {}", packages.join(", "));
        // `args` borrows the package names, so the owned set is cloned into
        // the result.
        check_with(shell, &args, &workspace, packages.clone(), &scope)
    }
}

/// Run one `yg diagnostics check` invocation and shape the verdict. Exit
/// zero is a pass; a non-zero exit carries the diagnostics output. A command
/// that cannot run at all is a loud terminal activity failure.
fn check_with(
    shell: &Shell,
    args: &[&str],
    workspace: &Workspace,
    affected_modules: Vec<String>,
    scope: &str,
) -> Result<CheckResult, ActivityFailure> {
    match shell.run("yg", args, &workspace.path) {
        Ok(command_run) => {
            let verdict = if command_run.succeeded() {
                CheckVerdict::Pass
            } else {
                CheckVerdict::Fail {
                    diagnostics: command_run.output,
                }
            };
            Ok(CheckResult {
                verdict,
                affected_modules,
                checked_scope: scope.to_owned(),
            })
        }
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "yg diagnostics check: {}",
            failure.message()
        ))),
    }
}

/// `full_checks`: the authoritative gate — the full workspace diagnostics
/// run, stricter than the fast scoped inner loop.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the gate scope has no implementation or
/// when `yg` cannot run. A failing workspace sweep is NOT an error — it is
/// the recorded fail verdict carrying the report.
pub fn full_checks(shell: &Shell, input: GateInput) -> Result<GateResult, ActivityFailure> {
    let GateInput {
        workspace, scope, ..
    } = input;
    match scope {
        GateScope::WorkspaceWide => {
            match shell.run(
                "yg",
                &["diagnostics", "check", "--workspace", "--format", "json"],
                &workspace.path,
            ) {
                Ok(command_run) => {
                    let verdict = if command_run.succeeded() {
                        GateVerdict::Pass
                    } else {
                        GateVerdict::Fail {
                            report: command_run.output,
                        }
                    };
                    Ok(GateResult { verdict })
                }
                Err(failure) => Err(ActivityFailure::terminal(format!(
                    "yg diagnostics check --workspace: {}",
                    failure.message()
                ))),
            }
        }
        // The affected-closure gate scope is a typed seam only — nothing
        // guessed until the graph-derived closure is trusted.
        GateScope::AffectedClosure { .. } => Err(ActivityFailure::terminal(
            "affected-closure gate scope has no local implementation \
             (TODO(meridian): complete affected closure from the workspace graph)",
        )),
    }
}

/// `request_review`: notify reviewers via collective DM that work is ready
/// for review. The verdict arrives later on the `review_verdict` signal.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the collective send command cannot run
/// or any reviewer DM fails to send.
pub fn request_review(shell: &Shell, input: &ReviewRequest) -> Result<ReviewAck, ActivityFailure> {
    let ReviewRequest {
        workspace,
        brief_id,
        reviewers,
        dev_result,
        gate_result,
        workflow_id,
    } = input;

    let signal_cmd = format!(
        "aion signal {workflow_id} review_verdict --payload '{{\"decision\":\"approve\"}}'"
    );

    let gate_status = match &gate_result.verdict {
        GateVerdict::Pass => "passed",
        GateVerdict::Fail { .. } => "failed",
    };

    let message = format!(
        "Brief {brief_id} is ready for review.\n\n\
         Branch: {branch}\n\
         Worktree: {path}\n\
         Summary: {summary}\n\
         Files touched: {files}\n\
         Gate: {gate_status}\n\n\
         To approve:\n{signal_cmd}\n\n\
         To request changes:\naion signal {workflow_id} review_verdict --payload \
         '{{\"decision\":\"request_changes\",\"notes\":[{{\"note\":\"your feedback here\"}}]}}'",
        branch = workspace.branch,
        path = workspace.path,
        summary = dev_result.summary,
        files = dev_result.files_touched.join(", "),
    );

    for reviewer in reviewers {
        require_run(
            shell,
            "collective",
            &[
                "send",
                "--as",
                "Meridian",
                "--to",
                reviewer,
                "--subject",
                &format!("Review: {brief_id}"),
                "--message",
                &message,
            ],
            ".",
            &format!("collective send to {reviewer}"),
        )?;
    }

    Ok(ReviewAck {
        request_id: workspace.branch.clone(),
    })
}

/// `land`: commit the dev rounds' files on the branch, then the yg-level
/// stack operation — merge the branch into its tree parent. Local, no PR
/// machinery.
///
/// Confirmed live (2026-06-13): the dev rounds leave norn's work UNCOMMITTED
/// in the worktree and `yg branch merge` merges committed work only, so
/// landing commits first. The merge runs from the MAIN repository:
/// `yg branch merge` removes the branch's worktree as part of landing — run
/// from inside the worktree it deletes its own git context mid-merge and
/// dies.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `git` or `yg` cannot run, when the
/// commit exits non-zero (including a dev round that changed nothing —
/// landing a no-op is an error, never a silent empty merge), or when the
/// merge exits non-zero.
pub fn land(shell: &Shell, input: LandInput) -> Result<Landed, ActivityFailure> {
    let LandInput {
        workspace,
        repo_root,
        base_ref,
        dev_result,
    } = input;
    require_run(shell, "git", &["add", "-A"], &workspace.path, "git add")?;
    let message = format!("{}: {}", workspace.branch, dev_result.summary);
    require_run(
        shell,
        "git",
        &["commit", "-m", &message],
        &workspace.path,
        "git commit",
    )?;
    require_run(
        shell,
        "yg",
        &["branch", "merge", &workspace.branch, "--yes"],
        &repo_root,
        "yg branch merge",
    )?;
    Ok(Landed {
        branch: workspace.branch,
        merged_into: base_ref,
    })
}

/// `enrich_brief`: append one stage report or the execution block into the
/// brief file inside the run's worktree (ADR-007, ADR-009), mirroring
/// `locals.enrich_brief`. The write is guarded by CN3: the on-disk brief's
/// authored subset must equal the handed document's before anything is
/// written — divergence is a terminal failure naming the brief path and the
/// first divergent field, never a silent overwrite. A missing, unreadable, or
/// undecodable brief file is a broken worktree: a can't-execute condition that
/// fails terminally (CN5), never a retry or a skip.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the brief file cannot be read or decoded,
/// when an authored field diverges, when a report names an unknown
/// requirement, or when the merged document cannot be written.
pub fn enrich_brief(_shell: &Shell, input: EnrichInput) -> Result<BriefDocument, ActivityFailure> {
    let EnrichInput {
        workspace,
        document,
        enrichment,
    } = input;
    // The design-system layout is a format constraint (briefs/ is what
    // validate.py keys its brief-schema detection on), so the path derives
    // from the handed document — never from a workflow-supplied guess.
    let brief_path: PathBuf = [
        workspace.path.as_str(),
        "docs",
        "design",
        document.cluster.as_str(),
        "briefs",
        &format!("{}.json", document.id),
    ]
    .iter()
    .collect();
    let brief_path_display = brief_path.display().to_string();

    let raw = std::fs::read_to_string(&brief_path).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: cannot read {brief_path_display}: {source}"
        ))
    })?;
    let on_disk: BriefDocument = serde_json::from_str(&raw).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: brief file {brief_path_display} failed to decode: {source}"
        ))
    })?;
    if let Some(field) = crate::enrich::authored_divergence(&on_disk, &document) {
        return Err(ActivityFailure::terminal(format!(
            "enrich_brief: authored field {field} in {brief_path_display} \
             diverges from the handed document; refusing to write (CN3)"
        )));
    }
    let merged = crate::enrich::apply(document, &enrichment)
        .map_err(|error| ActivityFailure::terminal(format!("enrich_brief: {error}")))?;
    let encoded = serde_json::to_string(&merged).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: cannot encode merged document: {source}"
        ))
    })?;
    std::fs::write(&brief_path, encoded).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: cannot write {brief_path_display}: {source}"
        ))
    })?;
    Ok(merged)
}

/// `assemble_wave`: resolve, order, and refuse a dispatch wave (BD-006),
/// mirroring `locals.assemble_wave`. The ledger-reading, reference-resolving
/// logic lives in [`crate::assemble`] (the only such code in the worker, CN1);
/// this is the handler seam the worker registers. It takes no `Shell` — like
/// the local, it performs file IO directly and shells nothing.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] on a refusal or any can't-execute condition
/// (unreadable ledger, undecodable brief, dependency-blocked, coverage-broken,
/// or cyclic wave).
pub fn assemble_wave(
    _shell: &Shell,
    input: AssembleInput,
) -> Result<AssembledWave, ActivityFailure> {
    crate::assemble::assemble_wave(input)
}

/// `teardown_workspace`: reclaim build cache from a provisioned workspace.
///
/// Preserves the worktree, git branch, and norn session logs so failed
/// workflows leave their artifacts intact for inspection and diagnosis.
/// Only runs `cargo clean` to free the build cache disk space.
///
/// # Errors
///
/// Never fails terminally — a `cargo clean` failure is logged but not
/// propagated, since the teardown is best-effort cleanup on an error path.
pub fn teardown_workspace(
    shell: &Shell,
    input: TeardownInput,
) -> Result<TornDown, ActivityFailure> {
    let branch = input.workspace.branch;

    if std::path::Path::new(&input.workspace.path).exists() {
        let _ = shell.run("cargo", &["clean"], &input.workspace.path);
    }

    Ok(TornDown {
        branch,
        cleaned: true,
    })
}

// --- helpers ----------------------------------------------------------------

/// Find a norn session ID that isn't already in use. Tries the base ID first,
/// then appends `-attempt-2`, `-attempt-3`, etc. Norn sessions persist at
/// `~/.norn/sessions/<id>.jsonl`; a stale session from a previous failed run
/// blocks reuse of the same ID.
/// Require a command to run AND exit zero; anything else is a terminal
/// activity failure carrying the command's diagnostics. Wording identical to
/// `locals.require_run`.
fn require_run(
    shell: &Shell,
    executable: &str,
    args: &[&str],
    cwd: &str,
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    require_run_with_envs(shell, executable, args, cwd, &[], context)
}

fn require_run_with_envs(
    shell: &Shell,
    executable: &str,
    args: &[&str],
    cwd: &str,
    envs: &[(&str, &str)],
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    match shell.run_with_envs(executable, args, cwd, envs) {
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
/// CONFIRMED against real norn (live run, 2026-06-13): `--output-format json`
/// emits a completion envelope with the schema-constrained result under
/// `"output"`, alongside `usage`/`model`/`session_id`/`events` (ignored
/// here). Exactly two shapes are attempted — the bare report (the fake-CLI
/// shims emit it raw), then norn's `{"output": <report>}` envelope — and if
/// BOTH fail the activity fails terminally carrying the head of the output. No
/// silent fallback beyond that documented two-shape attempt (C31).
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
