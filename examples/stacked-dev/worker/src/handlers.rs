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

use aion_worker::ActivityFailure;
use serde::Deserialize;

use crate::shell::{CliRun, Shell};
use crate::types::{
    CheckResult, CheckVerdict, DevInput, DevResult, GateInput, GateResult, GateScope, GateVerdict,
    Isolation, LandInput, Landed, ProvisionInput, ResumeInput, ReviewAck, ReviewRequest,
    ScopedInput, StartupResult, StartupTask, Workspace,
};

/// The JSON Schema norn structures the dev/resume result against — the
/// `DevResult` shape. Passed inline to `--output-schema` so there is no
/// schema file to resolve in the workspace. Byte-identical to
/// `locals.dev_output_schema`.
const DEV_OUTPUT_SCHEMA: &str = "{\"type\":\"object\",\
\"required\":[\"session_id\",\"files_touched\",\"summary\"],\
\"additionalProperties\":false,\
\"properties\":{\
\"session_id\":{\"type\":\"string\"},\
\"files_touched\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}},\
\"summary\":{\"type\":\"string\"}}}";

/// How much of an unparseable norn stdout rides in the terminal failure
/// message. Presentational truncation only: enough to diagnose the envelope
/// shape without shipping megabytes of agent transcript through the failure
/// payload.
const UNPARSEABLE_OUTPUT_HEAD: usize = 1000;

/// `provision_workspace`: provision an isolated workspace via the `yg` CLI.
///
/// Only worktree isolation has an implementation; the other typed variants
/// fail loudly, exactly like the local implementation's seam.
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
    // Two real yg verbs: add the branch as a child of the base ref, then
    // provision its worktree at a known absolute path (built from the repo
    // root) so every downstream activity holds a real directory and never a
    // cwd-relative guess.
    let branch = format!("stacked-dev-{}", input.brief_id);
    let worktree_path = format!("{}/.yggdrasil-worktrees/{branch}", input.repo_root);

    require_run(
        shell,
        "yg",
        &["branch", "add", &branch, &input.base_ref],
        &input.repo_root,
        "yg branch add",
    )?;
    // An explicit --path keeps the worktree location known a priori, never
    // parsed out of human output.
    require_run(
        shell,
        "yg",
        &["branch", "provision", &branch, "--path", &worktree_path],
        &input.repo_root,
        "yg branch provision",
    )?;
    Ok(Workspace {
        path: worktree_path,
        branch,
        placement: input.placement,
        isolation: input.isolation,
    })
}

/// Serve one startup fan-out task: the advisory warm build or the dev round.
/// Registered for BOTH the `warm_build` and `dev` activity names — the two
/// activities flow through one homogeneous `workflow.all` fan-out, so they
/// share the tagged `StartupTask`/`StartupResult` envelope.
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

/// Run the dev agent against the brief via the `norn` CLI.
fn dev(shell: &Shell, input: &DevInput) -> Result<StartupResult, ActivityFailure> {
    // The session id is deterministic (the branch name), so resume rounds
    // target the same session without ever capturing a generated id.
    let session_id = input.workspace.branch.clone();
    let prompt = dev_prompt(input);

    // norn takes the prompt positionally; --print is headless, --session-id
    // mints exactly this id, --workspace-root confines file tools,
    // --output-schema constrains the structured result, and
    // --output-format json emits the final envelope we decode.
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--session-id",
            &session_id,
            "--workspace-root",
            &input.workspace.path,
            "--output-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &prompt,
        ],
        &input.workspace.path,
        "norn dev",
    )?;
    let dev_result = parse_dev_result(&command_run, "norn dev")?;
    Ok(StartupResult::Developed {
        dev_result: DevResult {
            session_id,
            ..dev_result
        },
    })
}

/// Assemble the dev prompt from the brief and its design context, identical
/// to `locals.dev_prompt`.
fn dev_prompt(input: &DevInput) -> String {
    [
        "Implement the following brief in this workspace.".to_owned(),
        format!("## Brief\n{}", input.brief),
        format!("## Design\n{}", input.design),
        format!("## Checklist\n{}", input.checklist),
        format!("## Stories\n{}", input.stories.join("\n")),
        "Return your structured result matching the output schema.".to_owned(),
    ]
    .join("\n\n")
}

/// `dev_resume`: resume the same dev agent session with feedback
/// (scoped-check diagnostics or encoded review notes).
pub fn dev_resume(shell: &Shell, input: ResumeInput) -> Result<DevResult, ActivityFailure> {
    // Resume by the deterministic session id; the feedback is the prompt.
    // Like the local implementation, resume carries no --workspace-root (the
    // workspace root is not on ResumeInput yet — TODO(meridian) in locals).
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--resume",
            &input.session_id,
            "--output-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &input.feedback,
        ],
        ".",
        "norn resume",
    )?;
    let dev_result = parse_dev_result(&command_run, "norn resume")?;
    Ok(DevResult {
        session_id: input.session_id,
        ..dev_result
    })
}

/// `scoped_checks`: compute the affected package set from the dependency
/// graph, then run diagnostics limited to it. An empty affected set falls
/// back LOUDLY to a named workspace-wide scope; zero checks are never run
/// silently.
pub fn scoped_checks(shell: &Shell, input: ScopedInput) -> Result<CheckResult, ActivityFailure> {
    // Affected packages come from the dependency graph: one bare crate name
    // per line (direct-only = the crates that actually contain the changed
    // files; the gate runs broad).
    let mut affected_args = vec!["graph", "affected", "--plain", "--direct-only"];
    affected_args.extend(input.files_touched.iter().map(String::as_str));
    let affected_run = require_run(
        shell,
        "yg",
        &affected_args,
        &input.workspace.path,
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
            &input.workspace,
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
        check_with(shell, &args, &input.workspace, packages.clone(), &scope)
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
pub fn full_checks(shell: &Shell, input: GateInput) -> Result<GateResult, ActivityFailure> {
    match input.scope {
        GateScope::WorkspaceWide => {
            match shell.run(
                "yg",
                &["diagnostics", "check", "--workspace", "--format", "json"],
                &input.workspace.path,
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

/// `request_review`: emit a review request. It only requests — the verdict
/// arrives later on the `review_verdict` signal.
pub fn request_review(shell: &Shell, input: ReviewRequest) -> Result<ReviewAck, ActivityFailure> {
    let command_run = require_run(
        shell,
        "meridian",
        &[
            "review",
            "request",
            "--workspace",
            &input.workspace.path,
            "--brief-id",
            &input.brief_id,
            "--summary",
            &input.dev_result.summary,
        ],
        &input.workspace.path,
        "meridian review request",
    )?;
    let acked: RequestIdField = require_json(&command_run, "meridian review request")?;
    Ok(ReviewAck {
        request_id: acked.request_id,
    })
}

/// `land`: stack submit, then stack land. Never a manual cherry-pick or
/// merge.
pub fn land(shell: &Shell, input: LandInput) -> Result<Landed, ActivityFailure> {
    let submit_run = require_run(
        shell,
        "meridian",
        &["stack", "submit"],
        &input.workspace.path,
        "meridian stack submit",
    )?;
    let submitted: PrUrlField = require_json(&submit_run, "meridian stack submit")?;
    let land_run = require_run(
        shell,
        "meridian",
        &["stack", "land"],
        &input.workspace.path,
        "meridian stack land",
    )?;
    let landed: MergeCommitField = require_json(&land_run, "meridian stack land")?;
    Ok(Landed {
        pr_url: submitted.pr_url,
        merge_commit: landed.merge_commit,
    })
}

// --- helpers ----------------------------------------------------------------

/// `{"request_id": ..}` field of the review-request output (extra fields
/// tolerated, like the Gleam field decoder).
#[derive(Deserialize)]
struct RequestIdField {
    request_id: String,
}

/// `{"pr_url": ..}` field of the stack-submit output.
#[derive(Deserialize)]
struct PrUrlField {
    pr_url: String,
}

/// `{"merge_commit": ..}` field of the stack-land output.
#[derive(Deserialize)]
struct MergeCommitField {
    merge_commit: String,
}

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

/// Decode a command's stdout as JSON; malformed output is a terminal
/// activity failure carrying the raw text, like `locals.require_json`.
fn require_json<T: serde::de::DeserializeOwned>(
    command_run: &CliRun,
    context: &str,
) -> Result<T, ActivityFailure> {
    let trimmed = command_run.output.trim();
    serde_json::from_str(trimmed).map_err(|_| {
        ActivityFailure::terminal(format!("{context} produced unparseable output: {trimmed}"))
    })
}

/// Decode a norn command's stdout as a `DevResult`.
///
/// norn's `--output-format json` envelope shape is an open TODO(meridian) in
/// the local implementations: the structured `DevResult` may be the envelope
/// root or nested under a `result` field. Exactly two shapes are attempted —
/// the bare `DevResult`, then a `{"result": <DevResult>}` envelope — and if
/// BOTH fail the activity fails terminally carrying the head of the output.
/// No silent fallback beyond that documented two-shape attempt. The caller
/// overrides `session_id` with the id it set regardless.
fn parse_dev_result(command_run: &CliRun, context: &str) -> Result<DevResult, ActivityFailure> {
    let trimmed = command_run.output.trim();
    if let Ok(dev_result) = serde_json::from_str::<DevResult>(trimmed) {
        return Ok(dev_result);
    }

    #[derive(Deserialize)]
    struct ResultEnvelope {
        result: DevResult,
    }
    if let Ok(envelope) = serde_json::from_str::<ResultEnvelope>(trimmed) {
        return Ok(envelope.result);
    }

    Err(ActivityFailure::terminal(format!(
        "{context} produced unparseable output \
         (tried the bare DevResult shape and the {{\"result\": …}} envelope): {}",
        head(trimmed, UNPARSEABLE_OUTPUT_HEAD)
    )))
}

/// First `limit` characters of `text`, truncated on a char boundary.
fn head(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((boundary, _)) => &text[..boundary],
        None => text,
    }
}
