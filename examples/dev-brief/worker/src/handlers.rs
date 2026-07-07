//! The three SHELL activity handler bodies: `provision_workspace`,
//! `run_gates`, `cleanup_workspace`. Each shells to real `git` and the
//! brief's configured gate commands through [`crate::shell::Shell`].
//!
//! Failure taxonomy (the remediation/pipeline-run discipline, kept): a
//! command that cannot RUN (missing executable, dead working directory) is
//! INFRASTRUCTURE failure — a terminal [`ActivityFailure`], because retrying
//! a broken environment cannot help. A gate command whose non-zero exit is a
//! CONTRACT verdict (a red clippy, a failing suite) is RECORDED DATA returned
//! as a successful activity result — never an error, so the exit status lands
//! in durable workflow history. Nothing is ever swallowed into a success:
//! every red command rides back with its captured output.
//!
//! The bodies are plain synchronous `(&Shell, Input) -> Result<Output, _>` so
//! the hermetic tests drive them directly; `main.rs` adapts them onto the
//! worker's async handler signature.

use std::path::Path;

use aion_worker::ActivityFailure;

use crate::commit;
use crate::shell::{CliRun, Shell};
use crate::types::{
    CleanupInput, CleanupOutcome, GateCommandRun, GateInput, GateOutcome, ProvisionInput,
    WorkspaceInfo,
};

/// The base directory brief worktrees live under. MUST match
/// `dev_brief.workspace_base` in the Gleam workflow, because the workflow
/// derives each brief's `workspace_path` from it and the driven developer
/// harness points Norn's `--workspace-root` at the same
/// `<base>/{workflow_id}`.
pub const WORKSPACE_BASE: &str = "/tmp/aion-dev/ws";

/// Evidence clip bound: capture is kept whole below this, else head + tail
/// with an explicit truncation marker (durable history should carry the
/// verdict's shape, not megabytes of cargo spew; the marker keeps the cut
/// honest).
const CLIP_LIMIT: usize = 16_384;

// --- provision_workspace ------------------------------------------------------

/// `provision_workspace`: create the brief's isolated git worktree at
/// `workspace_path`, checking out `branch` freshly based on `base_branch`,
/// and report the base commit the gate will diff against.
///
/// Idempotent across retries: a pre-existing worktree at the path is removed
/// first, and `-B` resets the branch to `base_branch`, so a re-dispatch after
/// a crash lands in a clean, correctly-based worktree.
///
/// # Errors
///
/// Terminal when `git` cannot run, the repo is missing, or the worktree
/// cannot be created — provisioning is pure infrastructure.
pub fn provision(shell: &Shell, input: ProvisionInput) -> Result<WorkspaceInfo, ActivityFailure> {
    ensure_parent_dir(&input.workspace_path)?;

    // Best-effort removal of any stale worktree at this path (ignore failure:
    // it usually means "nothing was there").
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
            &input.branch,
            &input.workspace_path,
            &input.base_branch,
        ],
        &input.repo_root,
        "git worktree add",
    )?;

    // The exact commit the worktree starts from. Run through the parent repo
    // with -C so the shell's cwd check never couples to the new directory.
    let head = require_run(
        shell,
        "git",
        &["-C", &input.workspace_path, "rev-parse", "HEAD"],
        &input.repo_root,
        "git rev-parse HEAD (provisioned base)",
    )?;

    Ok(WorkspaceInfo {
        workspace_path: input.workspace_path,
        branch: input.branch,
        base_commit: head.stdout.trim().to_owned(),
    })
}

// --- run_gates ----------------------------------------------------------------------

/// `run_gates`: the brief's configured gate battery, FULLY MECHANICAL.
///
/// 1. Every configured command runs in order in the workspace root; pass =
///    exit 0; every run's exit status and clipped output are recorded. All
///    commands always run so a loop-back carries the full picture.
/// 2. A command that MUTATES the worktree (a write-mode formatter is the
///    expected case) leaves normalization the developer never saw: it is
///    committed mechanically under the machinery identity so the branch stays
///    complete and cleanup can remove a clean worktree.
/// 3. The reviewers' diff is captured as worktree-vs-`base_commit`, so
///    committed AND (any residual) uncommitted work are both visible.
/// 4. An EMPTY battery is the operator's explicit choice: a recorded vacuous
///    pass, named in `diagnostics`, never silent.
///
/// # Errors
///
/// Terminal only when a command cannot RUN at all or `git` fails as
/// infrastructure.
pub fn run_gates(shell: &Shell, input: GateInput) -> Result<GateOutcome, ActivityFailure> {
    let GateInput {
        workspace_path,
        base_commit,
        gates,
    } = input;
    let workspace = &workspace_path;
    let mut runs: Vec<GateCommandRun> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();

    for gate in &gates {
        let Some((executable, rest)) = gate.argv.split_first() else {
            // An empty argv can never run and can never be a verdict: a
            // configuration fault, surfaced loudly.
            return Err(ActivityFailure::terminal(format!(
                "gate `{}` has an empty argv — nothing to execute",
                gate.name
            )));
        };
        let args: Vec<&str> = rest.iter().map(String::as_str).collect();
        let run = require_can_run(
            shell,
            executable,
            &args,
            workspace,
            &format!("gate `{}`", gate.name),
        )?;
        let passed = run.succeeded();
        if !passed {
            diagnostics.push(format!(
                "gate `{}` exited {}:\n{}",
                gate.name,
                run.exit_status,
                clip(run.output.trim())
            ));
        }
        runs.push(GateCommandRun {
            name: gate.name.clone(),
            exit_code: i64::from(run.exit_status),
            passed,
            output_tail: clip(&run.output),
        });
    }

    // Gate commands may normalize the tree (cargo fmt in write mode is the
    // recommended first gate). That change belongs on the branch: commit it
    // mechanically so the branch is complete and the final cleanup sees a
    // clean worktree. Nothing dirty skips green.
    let normalization =
        commit::commit_gate_normalization(shell, workspace).map_err(ActivityFailure::terminal)?;
    if let commit::FixCommitOutcome::Committed { commit, paths } = &normalization {
        diagnostics.push(format!(
            "gate commands normalized the tree; committed mechanically as \
             {commit} ({} path(s))",
            paths.len()
        ));
    }

    // The developer's full change for the reviewers: worktree vs the base
    // commit, so anything uncommitted is visible too. --no-ext-diff pins the
    // plain unified format — an operator's difftastic/pager config must never
    // shape durable workflow evidence.
    let diff = require_run(
        shell,
        "git",
        &["-C", workspace, "diff", "--no-ext-diff", &base_commit],
        workspace,
        "git diff (reviewer evidence)",
    )?;

    let pass = runs.iter().all(|run| run.passed);
    let ran = runs.len();
    Ok(GateOutcome {
        pass,
        runs,
        diff: clip(&diff.stdout),
        diagnostics: if pass {
            if ran == 0 {
                "no gates configured — vacuous pass (the operator's explicit \
                 choice; nothing was verified mechanically)"
                    .to_owned()
            } else {
                let mut lines = vec![format!("all {ran} gate command(s) green")];
                lines.extend(diagnostics);
                lines.join("\n\n")
            }
        } else {
            diagnostics.join("\n\n")
        },
    })
}

// --- cleanup_workspace -----------------------------------------------------------------

/// `cleanup_workspace`: remove the brief's worktree. The branch (and every
/// commit on it) remains in the repository.
///
/// A DIRTY worktree is never removed — `git worktree remove --force` would
/// destroy uncommitted work. It is left in place and reported.
///
/// # Errors
///
/// Terminal only when `git` cannot run at all.
pub fn cleanup(shell: &Shell, input: CleanupInput) -> Result<CleanupOutcome, ActivityFailure> {
    let CleanupInput {
        repo_root,
        workspace_path,
    } = input;
    if !Path::new(&workspace_path).is_dir() {
        return Ok(CleanupOutcome {
            removed: false,
            detail: format!("workspace not present, nothing to remove: {workspace_path}"),
        });
    }

    let status = require_run(
        shell,
        "git",
        &["-C", &workspace_path, "status", "--porcelain"],
        &repo_root,
        "git status (cleanup dirty check)",
    )?;
    if !status.stdout.trim().is_empty() {
        return Ok(CleanupOutcome {
            removed: false,
            detail: format!(
                "worktree has uncommitted changes; left in place to avoid \
                 destroying work:\n{}",
                clip(&status.output)
            ),
        });
    }

    let removal = require_can_run(
        shell,
        "git",
        &[
            "-C",
            &repo_root,
            "worktree",
            "remove",
            "--force",
            &workspace_path,
        ],
        &repo_root,
        "git worktree remove",
    )?;
    Ok(CleanupOutcome {
        removed: removal.succeeded(),
        detail: if removal.succeeded() {
            "worktree removed; branch retained".to_owned()
        } else {
            format!(
                "git worktree remove exited {}: {}",
                removal.exit_status,
                clip(removal.output.trim())
            )
        },
    })
}

// --- helpers --------------------------------------------------------------------------

/// Clip captured output to [`CLIP_LIMIT`], keeping the head and tail around
/// an explicit marker so a cut is never silent.
#[must_use]
pub fn clip(text: &str) -> String {
    if text.len() <= CLIP_LIMIT {
        return text.to_owned();
    }
    let head_len = CLIP_LIMIT / 4;
    let tail_len = CLIP_LIMIT - head_len;
    let head_end = floor_char_boundary(text, head_len);
    let tail_start = floor_char_boundary(text, text.len() - tail_len);
    format!(
        "{}\n[... {} bytes truncated ...]\n{}",
        &text[..head_end],
        text.len() - head_end - (text.len() - tail_start),
        &text[tail_start..]
    )
}

/// The largest char boundary at or below `index` (a stable-Rust stand-in for
/// `str::floor_char_boundary`).
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut boundary = index.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

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

/// Require a command to run AND exit zero; anything else is a terminal
/// failure (infrastructure, not a contract verdict).
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
            clip(run.output.trim())
        ))),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "{context}: {}",
            failure.message()
        ))),
    }
}

/// Require a command to merely RUN; a non-zero exit is recorded data the
/// caller interprets, never an error. Only an unrunnable command is terminal.
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
