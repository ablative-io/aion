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
    DevResult, EnrichInput, GateInput, GateResult, GateScope, GateVerdict, Isolation, LandInput,
    Landed, Placement, ProvisionInput, ResumeInput, ReviewAck, ReviewInput, ReviewReport,
    ReviewRequest, ScopedInput, ScoutInput, ScoutReport, StartupResult, StartupTask, TeardownInput,
    TornDown, Workspace,
};

/// How much of an unparseable norn stdout rides in the terminal failure
/// message. Presentational truncation only: enough to diagnose the envelope
/// shape without shipping megabytes of agent transcript through the failure
/// payload.
const UNPARSEABLE_OUTPUT_HEAD: usize = 1000;

/// `provision_workspace`: provision an isolated workspace.
///
/// Local placement provisions a yg worktree; remote placement clones from
/// the `clone_url` and creates a branch. Only worktree isolation has a
/// local implementation; the other typed variants fail loudly.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the isolation mode has no
/// implementation, when the provisioning CLI cannot run, or when any verb
/// exits non-zero.
pub fn provision_workspace(
    shell: &Shell,
    input: ProvisionInput,
) -> Result<Workspace, ActivityFailure> {
    match input.placement {
        Placement::Remote => provision_clone(shell, input),
        Placement::Local => match input.isolation {
            Isolation::Worktree => provision_worktree(shell, input),
            Isolation::Copy | Isolation::Overlay | Isolation::Vm => {
                Err(ActivityFailure::terminal(format!(
                    "isolation mode {} is a typed seam with no local implementation \
                     (TODO(meridian): exchange-VM dispatch)",
                    input.isolation.wire_name()
                )))
            }
        },
    }
}

fn provision_worktree(shell: &Shell, input: ProvisionInput) -> Result<Workspace, ActivityFailure> {
    let ProvisionInput {
        repo_root,
        brief_id,
        base_ref,
        placement,
        isolation,
        clone_url: _,
    } = input;

    let branch = resolve_branch_name(shell, &repo_root, &brief_id, &base_ref)?;
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

/// Try `stacked-dev-{brief_id}`, then `-attempt-2`, `-attempt-3`, etc.
/// when a previous run left a branch behind.
fn resolve_branch_name(
    shell: &Shell,
    repo_root: &str,
    brief_id: &str,
    base_ref: &str,
) -> Result<String, ActivityFailure> {
    let base_branch = format!("stacked-dev-{brief_id}");
    match try_branch_add(shell, repo_root, &base_branch, base_ref)? {
        BranchAddOutcome::Created => return Ok(base_branch),
        BranchAddOutcome::AlreadyExists => {}
    }

    let mut attempt = 2u32;
    loop {
        let branch = format!("stacked-dev-{brief_id}-attempt-{attempt}");
        match try_branch_add(shell, repo_root, &branch, base_ref)? {
            BranchAddOutcome::Created => return Ok(branch),
            BranchAddOutcome::AlreadyExists => {}
        }
        attempt = attempt.saturating_add(1);
    }
}

enum BranchAddOutcome {
    Created,
    AlreadyExists,
}

fn try_branch_add(
    shell: &Shell,
    repo_root: &str,
    branch: &str,
    base_ref: &str,
) -> Result<BranchAddOutcome, ActivityFailure> {
    match shell.run("yg", &["branch", "add", branch, base_ref], repo_root) {
        Ok(run) if run.succeeded() => Ok(BranchAddOutcome::Created),
        Ok(run) if run.combined_output().contains("already exists") => {
            Ok(BranchAddOutcome::AlreadyExists)
        }
        Ok(run) => Err(ActivityFailure::terminal(format!(
            "yg branch add failed — exit status {}: {}",
            run.exit_status,
            run.combined_output().trim()
        ))),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "yg branch add: {}",
            failure.message()
        ))),
    }
}

fn provision_clone(shell: &Shell, input: ProvisionInput) -> Result<Workspace, ActivityFailure> {
    let clone_url = input.clone_url.ok_or_else(|| {
        ActivityFailure::terminal(
            "remote placement requires a clone_url in the provision input".to_string(),
        )
    })?;
    let branch = resolve_clone_branch_name(&input.brief_id);
    let clone_path = format!("/tmp/stacked-dev-clones/{branch}");

    require_run(
        shell,
        "rm",
        &["-rf", &clone_path],
        "/tmp",
        "clean clone target",
    )?;
    require_run(
        shell,
        "git",
        &["clone", "--branch", &input.base_ref, &clone_url, &clone_path],
        "/tmp",
        "git clone",
    )?;
    require_run(
        shell,
        "git",
        &["checkout", "-b", &branch],
        &clone_path,
        "git checkout -b",
    )?;
    Ok(Workspace {
        path: clone_path,
        branch,
        placement: input.placement,
        isolation: input.isolation,
    })
}

fn resolve_clone_branch_name(brief_id: &str) -> String {
    let base = format!("stacked-dev-{brief_id}");
    let clone_dir = "/tmp/stacked-dev-clones";
    let base_path = format!("{clone_dir}/{base}");
    if !std::path::Path::new(&base_path).exists() {
        return base;
    }
    for attempt in 2..=10 {
        let branch = format!("stacked-dev-{brief_id}-attempt-{attempt}");
        let path = format!("{clone_dir}/{branch}");
        if !std::path::Path::new(&path).exists() {
            return branch;
        }
    }
    base
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

/// Run the dev agent against the projected dev prompt via the `norn` CLI.
fn dev(shell: &Shell, input: &DevInput) -> Result<StartupResult, ActivityFailure> {
    // The session id is deterministic (the branch name), so resume rounds
    // target the same session without ever capturing a generated id.
    let session_id = input.workspace.branch.clone();

    // norn takes the projected prompt positionally; --print is headless,
    // --session-id mints exactly this id, --workspace-root confines file
    // tools, --output-schema constrains the structured result to the
    // dev-report shape, and --output-format json emits the final envelope we
    // decode.
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--fast",
            "--reasoning-effort",
            "high",
            "--session-id",
            &session_id,
            "--workspace-root",
            &input.workspace.path,
            "--output-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &input.prompt,
        ],
        &input.workspace.path,
        "norn dev",
    )?;
    let dev_report = parse_report::<DevReport>(&command_run, "norn dev")?;
    Ok(StartupResult::Developed { dev_report })
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
            "--fast",
            "--reasoning-effort",
            "high",
            "--session-id",
            &session_id,
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
            "--fast",
            "--reasoning-effort",
            "high",
            "--session-id",
            &session_id,
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

/// `dev_resume`: resume the same dev agent session with feedback (scoped-check
/// diagnostics). Returns a FULL replacement dev report.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither the bare report nor the `{"output":
/// …}` envelope.
pub fn dev_resume(shell: &Shell, input: ResumeInput) -> Result<DevReport, ActivityFailure> {
    // Resume by the deterministic session id; the feedback is the prompt.
    // Like the local implementation, resume carries no --workspace-root (the
    // workspace root is not on ResumeInput yet — TODO(meridian) in locals).
    let ResumeInput {
        session_id,
        feedback,
    } = input;
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--fast",
            "--reasoning-effort",
            "high",
            "--resume",
            &session_id,
            "--output-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &feedback,
        ],
        ".",
        "norn resume",
    )?;
    parse_report::<DevReport>(&command_run, "norn resume")
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
        .stdout
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
                    diagnostics: command_run.combined_output(),
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
                            report: command_run.combined_output(),
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
        clone_url: _,
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
    clean_build_artifacts(&workspace.path);
    match workspace.placement {
        Placement::Remote => land_remote(shell, &workspace, &base_ref, &dev_result),
        Placement::Local => {
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
    }
}

fn land_remote(
    shell: &Shell,
    workspace: &Workspace,
    base_ref: &str,
    dev_result: &DevResult,
) -> Result<Landed, ActivityFailure> {
    require_run(
        shell,
        "git",
        &["push", "-u", "origin", &workspace.branch],
        &workspace.path,
        "git push",
    )?;
    let title = format!("{}: {}", workspace.branch, dev_result.summary);
    let body = format!(
        "## Summary\n\n{}\n\n## Files changed\n\n{}\n\n\
         ---\nGenerated by stacked-dev workflow",
        dev_result.summary,
        dev_result
            .files_touched
            .iter()
            .map(|f| format!("- `{f}`"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    require_run(
        shell,
        "gh",
        &[
            "pr",
            "create",
            "--base",
            base_ref,
            "--head",
            &workspace.branch,
            "--title",
            &title,
            "--body",
            &body,
        ],
        &workspace.path,
        "gh pr create",
    )?;
    Ok(Landed {
        branch: workspace.branch.clone(),
        merged_into: format!("pr-into-{base_ref}"),
    })
}

pub fn teardown_workspace(
    shell: &Shell,
    input: TeardownInput,
) -> Result<TornDown, ActivityFailure> {
    let branch = input.workspace.branch;
    let workspace_path = input.workspace.path;

    clean_build_artifacts(&workspace_path);

    for suffix in ["", "-scout", "-review"] {
        let session = format!("{branch}{suffix}");
        let _ = shell.run("norn", &["session", "remove", &session], ".");
    }

    match input.workspace.placement {
        Placement::Local => {
            let _ = shell.run(
                "yg",
                &["branch", "teardown", &branch],
                &input.repo_root,
            );
            let _ = shell.run(
                "yg",
                &["branch", "remove", "--yes", &branch],
                &input.repo_root,
            );
            let _ = shell.run("git", &["branch", "-D", &branch], &input.repo_root);
        }
        Placement::Remote => {
            if std::path::Path::new(&workspace_path).is_dir() {
                let _ = std::fs::remove_dir_all(&workspace_path);
            }
        }
    }

    Ok(TornDown {
        branch,
        cleaned: true,
    })
}

fn clean_build_artifacts(workspace_path: &str) {
    let target = format!("{workspace_path}/target");
    if std::path::Path::new(&target).is_dir() {
        tracing::info!(path = %target, "removing build artifacts before landing");
        if let Err(error) = std::fs::remove_dir_all(&target) {
            tracing::warn!(path = %target, %error, "failed to remove build artifacts");
        }
    }
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
            command_run.combined_output().trim()
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
    let trimmed = command_run.stdout.trim();
    serde_json::from_str(trimmed).map_err(|_| {
        ActivityFailure::terminal(format!("{context} produced unparseable output: {trimmed}"))
    })
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

    for candidate in [trimmed, first_json_line(trimmed)] {
        if let Ok(report) = serde_json::from_str::<T>(candidate) {
            return Ok(report);
        }
        if let Ok(envelope) = serde_json::from_str::<NornEnvelope<T>>(candidate) {
            return Ok(envelope.output);
        }
    }

    let line = first_json_line(trimmed);
    let bare_err = match serde_json::from_str::<T>(line) {
        Err(e) => e,
        Ok(_) => unreachable!(),
    };
    let envelope_err = match serde_json::from_str::<NornEnvelope<T>>(line) {
        Err(e) => e,
        Ok(_) => unreachable!(),
    };

    Err(ActivityFailure::terminal(format!(
        "{context} produced unparseable output \
         (bare: {bare_err}; envelope: {envelope_err}): {}",
        head(trimmed, UNPARSEABLE_OUTPUT_HEAD)
    )))
}

fn first_json_line(text: &str) -> &str {
    text.lines()
        .find(|line| line.starts_with('{'))
        .unwrap_or(text)
}

/// First `limit` characters of `text`, truncated on a char boundary.
fn head(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((boundary, _)) => &text[..boundary],
        None => text,
    }
}
