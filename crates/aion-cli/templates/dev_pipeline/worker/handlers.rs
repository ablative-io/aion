//! Activity handler bodies, mirroring `../src/{{name}}/locals.gleam`
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
    // Two real yg verbs: add the branch as a child of the base ref, then
    // provision its worktree at a known absolute path (built from the repo
    // root) so every downstream activity holds a real directory and never a
    // cwd-relative guess.
    let branch = format!("{{name}}-{brief_id}");
    let worktree_path = format!("{repo_root}/.yggdrasil-worktrees/{branch}");

    require_run(
        shell,
        "yg",
        &["branch", "add", &branch, &base_ref],
        &repo_root,
        "yg branch add",
    )?;
    // An explicit --path keeps the worktree location known a priori, never
    // parsed out of human output.
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
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither documented `DevResult` shape.
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

/// `request_review`: emit a review request. It only requests — the verdict
/// arrives later on the `review_verdict` signal.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `meridian` cannot run, exits non-zero,
/// or answers without a parseable response envelope.
pub fn request_review(shell: &Shell, input: ReviewRequest) -> Result<ReviewAck, ActivityFailure> {
    // CONFIRMED against the real CLI (live runs, 2026-06-13):
    // `meridian review request <BRANCH> --reviewer <NAME>... --as Meridian`.
    // The branch positional must come FIRST: `--reviewer` is greedy
    // multi-value and swallows a trailing positional as another reviewer.
    // `--as` names the requesting identity — always the Meridian system
    // member (the CLI refuses to guess when the workspace has several
    // members). The meridian workspace resolves from the CLI's own global
    // config.
    let ReviewRequest {
        workspace,
        reviewers,
        ..
    } = input;
    let mut args: Vec<String> = vec![
        "review".to_owned(),
        "request".to_owned(),
        workspace.branch.clone(),
    ];
    for reviewer in &reviewers {
        args.push("--reviewer".to_owned());
        args.push(reviewer.clone());
    }
    args.push("--as".to_owned());
    args.push("Meridian".to_owned());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let command_run = require_run(
        shell,
        "meridian",
        &arg_refs,
        &workspace.path,
        "meridian review request",
    )?;
    // CONFIRMED against the real CLI (live run, 2026-06-13): the response
    // envelope is `{"branch": .., "reviewers": [{"name", "dm_status", ..}],
    // ..}` — there is no request id. The branch IS the request's identity
    // (meridian persists `pending_reviewers` against the branch lifecycle),
    // so the recorded ack carries it. Every requested reviewer must have
    // been notified (`dm_status: "sent"`); anything else fails loudly.
    let response: ReviewRequestResponse = require_json(&command_run, "meridian review request")?;
    if let Some(unsent) = response
        .reviewers
        .iter()
        .find(|reviewer| reviewer.dm_status != "sent")
    {
        return Err(ActivityFailure::terminal(format!(
            "meridian review request did not notify reviewer {}: dm_status was {:?}",
            unsent.name, unsent.dm_status
        )));
    }
    Ok(ReviewAck {
        request_id: response.branch,
    })
}

/// `land`: commit the dev rounds' files on the branch, then the yg-level
/// stack operation — merge the branch into its tree parent. Local, no PR
/// machinery.
///
/// Confirmed live (2026-06-13): the dev rounds leave the agent's work
/// UNCOMMITTED in the worktree and `yg branch merge` merges committed work
/// only, so landing commits first. The merge runs from the MAIN repository:
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

// --- helpers ----------------------------------------------------------------

/// The `meridian review request` response envelope — only the fields the
/// handler consumes (extra fields tolerated, like the Gleam field decoder).
#[derive(Deserialize)]
struct ReviewRequestResponse {
    branch: String,
    reviewers: Vec<ReviewerNotification>,
}

/// One reviewer's notification outcome inside [`ReviewRequestResponse`].
#[derive(Deserialize)]
struct ReviewerNotification {
    name: String,
    dm_status: String,
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
/// Exactly two shapes are attempted — the bare `DevResult` (what the
/// fake-CLI shims emit), then real norn's completion envelope with the
/// schema-constrained result under `"output"` — and if BOTH fail the
/// activity fails terminally carrying the head of the output. No silent
/// fallback beyond that documented two-shape attempt. The caller overrides
/// `session_id` with the id it set regardless.
fn parse_dev_result(command_run: &CliRun, context: &str) -> Result<DevResult, ActivityFailure> {
    // CONFIRMED against real norn (live run, 2026-06-13): `--output-format
    // json` emits a completion envelope with the schema-constrained result
    // under `"output"`, alongside `usage`/`model`/`session_id`/`events`
    // (ignored here). The bare shape stays first for the fake-CLI shims,
    // which emit the `DevResult` raw.
    #[derive(Deserialize)]
    struct NornEnvelope {
        output: DevResult,
    }

    let trimmed = command_run.output.trim();
    if let Ok(dev_result) = serde_json::from_str::<DevResult>(trimmed) {
        return Ok(dev_result);
    }
    if let Ok(envelope) = serde_json::from_str::<NornEnvelope>(trimmed) {
        return Ok(envelope.output);
    }

    // The envelope shape is spelled with a plain `{}` placeholder (not an
    // escaped brace literal) because this file doubles as an `aion new`
    // template, whose renderer treats a doubled opening brace as an
    // unresolved placeholder.
    Err(ActivityFailure::terminal(format!(
        "{context} produced unparseable output \
         (tried the bare DevResult shape and norn's {} envelope): {}",
        "{\"output\": …}",
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
