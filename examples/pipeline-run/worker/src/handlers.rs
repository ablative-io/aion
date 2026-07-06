//! The four SHELL activity handler bodies: `provision_workspace`, `gate`,
//! `land`, `notify`. Each shells to real `git`/`cargo`/`collective` through
//! [`crate::shell::Shell`].
//!
//! Failure taxonomy (`meridian_dev_pipeline` discipline): a command that cannot
//! RUN (missing executable, dead working directory) is INFRASTRUCTURE failure —
//! a terminal [`ActivityFailure`], because retrying a broken environment cannot
//! help. A command whose non-zero exit is a CONTRACT verdict (the cargo gate's
//! pass/fail, a merge conflict) is RECORDED DATA returned as a successful
//! activity result — never an error, so the exit status lands in durable
//! workflow history (rigid step 4).
//!
//! The bodies are plain synchronous `(&Shell, Input) -> Result<Output, _>` so
//! the hermetic tests drive them directly with fake-CLI shims on a private
//! `PATH`; `main.rs` adapts them onto the worker's async handler signature.

use std::path::Path;

use aion_worker::ActivityFailure;

use crate::shell::{CliRun, Shell};
use crate::types::{
    GateInput, GateOutcome, LandInput, LandOutcome, NotifyInput, NotifyOutcome, ProvisionInput,
    WorkspaceInfo,
};

/// The base directory unit worktrees and the integration worktree live under.
/// MUST match `pipeline_unit.workspace_base` in the Gleam workflow, because the
/// child derives each unit's `workspace_path` from it and the dev/review
/// harnesses point Norn's `--workspace-root` at the same `<base>/{workflow_id}`.
pub const WORKSPACE_BASE: &str = "/tmp/aion-pipeline-run/ws";

// --- provision_workspace ---------------------------------------------------

/// `provision_workspace`: create the unit's isolated git worktree at
/// `workspace_path`, checking out `unit_branch` freshly based on `base_branch`.
///
/// Idempotent across retries: a pre-existing worktree at the path is removed
/// first, and `-B` resets the branch to `base_branch`, so a re-dispatch after a
/// crash lands in a clean, correctly-based worktree rather than failing on
/// "already exists".
///
/// # Errors
///
/// Terminal when `git` cannot run, the repo is missing, or the worktree cannot
/// be created — provisioning is pure infrastructure, so any failure is terminal.
pub fn provision(shell: &Shell, input: ProvisionInput) -> Result<WorkspaceInfo, ActivityFailure> {
    ensure_parent_dir(&input.workspace_path)?;

    // Best-effort remove of any stale worktree at this path (ignore failure: it
    // usually means "nothing was there"), so the add below starts clean.
    let _ = shell.run(
        "git",
        &[
            "-C",
            &input.repo_root,
            "worktree",
            "remove",
            "--force",
            &input.workspace_path,
        ],
        &input.repo_root,
    );

    require_run(
        shell,
        "git",
        &[
            "-C",
            &input.repo_root,
            "worktree",
            "add",
            "--force",
            "-B",
            &input.unit_branch,
            &input.workspace_path,
            &input.base_branch,
        ],
        &input.repo_root,
        "git worktree add",
    )?;

    Ok(WorkspaceInfo {
        workspace_path: input.workspace_path,
        branch: input.unit_branch,
    })
}

// --- gate ------------------------------------------------------------------

/// `gate`: the cargo gate in the unit workspace — autoformat, then
/// `cargo clippy --workspace --all-targets -- -D warnings`, then
/// `cargo test --workspace`. Pass is both check commands exit zero; on fail the
/// combined output rides in `diagnostics`.
///
/// A non-zero cargo exit is the recorded FAIL verdict, not an activity error;
/// only cargo being unrunnable is terminal.
///
/// # Errors
///
/// Terminal when `cargo` cannot run at all.
pub fn gate(shell: &Shell, input: GateInput) -> Result<GateOutcome, ActivityFailure> {
    let workspace = input.workspace_path;

    // Autoformat first (write mode, never a check) — a formatting-only failure
    // must not fail the gate, so its outcome is ignored (rigid step 5).
    let _ = shell.run("cargo", &["fmt", "--all"], &workspace);

    let clippy = run_or_terminal(
        shell,
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &workspace,
        "cargo clippy",
    )?;
    if !clippy.succeeded() {
        return Ok(GateOutcome {
            pass: false,
            diagnostics: clippy.output,
        });
    }

    let test = run_or_terminal(
        shell,
        "cargo",
        &["test", "--workspace"],
        &workspace,
        "cargo test",
    )?;
    Ok(GateOutcome {
        pass: test.succeeded(),
        diagnostics: if test.succeeded() {
            String::new()
        } else {
            test.output
        },
    })
}

// --- land ------------------------------------------------------------------

/// `land`: merge the unit branches, in order, onto the integration branch.
///
/// The integration branch is materialized as its own worktree under
/// [`WORKSPACE_BASE`] (created from `base_branch` on the first land, reused
/// thereafter), so landing never disturbs a checked-out branch in `repo_root`.
/// Each unit branch is merged `--no-ff`. A merge that CONFLICTS is a contract
/// outcome, not an infrastructure error: the merge is aborted, landing stops at
/// that unit, and the result records what landed and why it stopped — so the
/// partial progress and the reason are both in durable history.
///
/// # Errors
///
/// Terminal only when `git` cannot run or the integration worktree cannot be
/// created.
pub fn land(shell: &Shell, input: LandInput) -> Result<LandOutcome, ActivityFailure> {
    let integration_ws = format!(
        "{WORKSPACE_BASE}/integration/{}",
        sanitize(&input.integration_branch)
    );
    ensure_parent_dir(&integration_ws)?;

    // Create-or-reuse the integration worktree. `-B` makes the first land seed
    // the integration branch from base_branch; a later land reuses the existing
    // integration branch state — so we do NOT pass `-B` when it already exists,
    // which would discard prior landed work. Distinguish by checking the ref.
    let integration_exists = branch_exists(shell, &input.repo_root, &input.integration_branch);
    // Remove any stale worktree registration at the path first (idempotent).
    let _ = shell.run(
        "git",
        &[
            "-C",
            &input.repo_root,
            "worktree",
            "remove",
            "--force",
            &integration_ws,
        ],
        &input.repo_root,
    );
    if integration_exists {
        require_run(
            shell,
            "git",
            &[
                "-C",
                &input.repo_root,
                "worktree",
                "add",
                "--force",
                &integration_ws,
                &input.integration_branch,
            ],
            &input.repo_root,
            "git worktree add (integration)",
        )?;
    } else {
        require_run(
            shell,
            "git",
            &[
                "-C",
                &input.repo_root,
                "worktree",
                "add",
                "--force",
                "-B",
                &input.integration_branch,
                &integration_ws,
                &input.base_branch,
            ],
            &input.repo_root,
            "git worktree add (integration seed)",
        )?;
    }

    let mut landed: Vec<String> = Vec::new();
    let mut details: Vec<String> = Vec::new();
    for unit in &input.units {
        // Every git command runs with cwd = repo_root (a directory known to
        // exist) and targets the integration worktree via `-C`, so the shell's
        // cwd check never couples to the worktree's on-disk creation.
        let merge = require_can_run(
            shell,
            "git",
            &[
                "-C",
                &integration_ws,
                "merge",
                "--no-ff",
                "--no-edit",
                &unit.branch,
            ],
            &input.repo_root,
            "git merge",
        )?;
        if merge.succeeded() {
            landed.push(unit.unit_id.clone());
            details.push(format!("{}: merged {}", unit.unit_id, unit.branch));
        } else {
            // A conflict (or other non-zero): abort and stop landing here.
            let _ = shell.run(
                "git",
                &["-C", &integration_ws, "merge", "--abort"],
                &input.repo_root,
            );
            details.push(format!(
                "{}: merge of {} did not apply cleanly; landing stopped ({})",
                unit.unit_id,
                unit.branch,
                merge.output.trim()
            ));
            break;
        }
    }

    Ok(LandOutcome {
        landed,
        integration_branch: input.integration_branch,
        detail: details.join("; "),
    })
}

// --- notify ----------------------------------------------------------------

/// `notify`: a best-effort completion notice. Always logs; additionally sends
/// through `collective send` when that CLI is on `PATH`. A missing `collective`
/// is NOT a failure — the notice is log-only and the result says so.
///
/// # Errors
///
/// Never terminal for a missing notifier; only a spawn error other than
/// not-found surfaces (kept simple: any run error degrades to log-only).
pub fn notify(shell: &Shell, input: NotifyInput) -> Result<NotifyOutcome, ActivityFailure> {
    let NotifyInput { brief_id, summary } = input;
    tracing::info!(brief = %brief_id, "pipeline-run complete: {summary}");

    let subject = format!("{brief_id} pipeline complete");
    match shell.run(
        "collective",
        &[
            "send",
            "--as",
            "Meridian",
            "--subject",
            &subject,
            "--message",
            &summary,
        ],
        ".",
    ) {
        Ok(run) if run.succeeded() => Ok(NotifyOutcome {
            sent: true,
            detail: "sent via collective".to_owned(),
        }),
        Ok(run) => Ok(NotifyOutcome {
            sent: false,
            detail: format!("collective send exited {}; logged instead", run.exit_status),
        }),
        Err(_) => Ok(NotifyOutcome {
            sent: false,
            detail: "collective not available; logged instead".to_owned(),
        }),
    }
}

// --- helpers ---------------------------------------------------------------

/// Ensure the PARENT directory of `path` exists (git worktree add needs it).
fn ensure_parent_dir(path: &str) -> Result<(), ActivityFailure> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ActivityFailure::terminal(format!(
                "could not create workspace parent directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    Ok(())
}

/// Whether `refs/heads/<branch>` exists in `repo_root`.
fn branch_exists(shell: &Shell, repo_root: &str, branch: &str) -> bool {
    let refspec = format!("refs/heads/{branch}");
    matches!(
        shell.run(
            "git",
            &["-C", repo_root, "show-ref", "--verify", "--quiet", &refspec],
            repo_root,
        ),
        Ok(run) if run.succeeded()
    )
}

/// Reduce an arbitrary branch name to a filesystem-safe directory segment.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Require a command to run AND exit zero; anything else is a terminal failure.
fn require_run(
    shell: &Shell,
    executable: &str,
    args: &[&str],
    cwd: &str,
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    match shell.run(executable, args, cwd) {
        Ok(run) if run.succeeded() => Ok(run),
        Ok(run) => Err(ActivityFailure::terminal(format!(
            "{context} failed — exit status {}: {}",
            run.exit_status,
            run.output.trim()
        ))),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "{context}: {}",
            failure.message()
        ))),
    }
}

/// Require a command to merely RUN; a non-zero exit is recorded data the caller
/// interprets, never an error. Only an unrunnable command is terminal.
fn require_can_run(
    shell: &Shell,
    executable: &str,
    args: &[&str],
    cwd: &str,
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    shell
        .run(executable, args, cwd)
        .map_err(|failure| ActivityFailure::terminal(format!("{context}: {}", failure.message())))
}

/// `require_can_run` under the name the gate uses for its cargo commands.
fn run_or_terminal(
    shell: &Shell,
    executable: &str,
    args: &[&str],
    cwd: &str,
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    require_can_run(shell, executable, args, cwd, context)
}
