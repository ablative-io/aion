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
use crate::paths;
use crate::shell::{CliRun, Shell};
use crate::types::{
    CleanupInput, CleanupOutcome, GateCommand, GateCommandRun, GateInput, GateOutcome,
    ProvisionInput, ResetInput, ResetOutcome, VerifyInput, VerifyOutcome, WorkspaceInfo,
};

/// Evidence clip bound: capture is kept whole below this, else head + tail
/// with an explicit truncation marker (durable history should carry the
/// verdict's shape, not megabytes of cargo spew; the marker keeps the cut
/// honest). The brief worktree base is no longer a worker constant — the
/// workflow derives each `workspace_path` from `config.repo_root` and carries
/// it on every activity input (see `dev_brief.worktrees_subdir`).
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

    // A PREVIOUS run of this brief may have abandoned a worktree elsewhere
    // (a failed run never reaches cleanup), and git refuses to reset a
    // branch checked out in another worktree — which would block every
    // re-run of the brief forever. Reclaim stale holders of this branch:
    // remove them when CLEAN; refuse loudly (naming the path) when dirty,
    // because uncommitted work must never be destroyed.
    reclaim_branch_worktrees(
        shell,
        &input.repo_root,
        &input.branch,
        &input.workspace_path,
    )?;

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

/// Find every OTHER worktree of `repo_root` holding `branch` and remove it
/// when clean; a dirty holder is a terminal error naming the path (operator
/// salvage beats silent destruction).
fn reclaim_branch_worktrees(
    shell: &Shell,
    repo_root: &str,
    branch: &str,
    own_workspace: &str,
) -> Result<(), ActivityFailure> {
    let listing = require_run(
        shell,
        "git",
        &["-C", repo_root, "worktree", "list", "--porcelain"],
        repo_root,
        "git worktree list (stale branch holders)",
    )?;
    let wanted_ref = format!("refs/heads/{branch}");
    let mut current_path: Option<String> = None;
    let mut holders: Vec<String> = Vec::new();
    for line in listing.stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.trim().to_owned());
        } else if let Some(branch_ref) = line.strip_prefix("branch ")
            && branch_ref.trim() == wanted_ref
            && let Some(path) = current_path.clone()
            && path != own_workspace
        {
            holders.push(path);
        }
    }
    for holder in holders {
        let status = require_run(
            shell,
            "git",
            &["-C", &holder, "status", "--porcelain"],
            repo_root,
            "git status (stale holder dirty check)",
        )?;
        if !status.stdout.trim().is_empty() {
            return Err(ActivityFailure::terminal(format!(
                "branch {branch} is held by the stale worktree {holder}, which \
                 has UNCOMMITTED changes — refusing to destroy work; salvage \
                 or remove it, then re-run"
            )));
        }
        require_run(
            shell,
            "git",
            &["-C", repo_root, "worktree", "remove", "--force", &holder],
            repo_root,
            "git worktree remove (stale clean holder)",
        )?;
        tracing::info!(%holder, %branch, "removed a stale clean worktree holding the brief branch");
    }
    Ok(())
}

// --- run_gates ----------------------------------------------------------------------

/// One gate command's RAW (untruncated) execution record: the resolved argv,
/// the exit status, and the full combined output. `run_gates` clips this into
/// the event's [`GateCommandRun`]; `verify_gates` additionally writes the full
/// output to its log file before clipping.
struct RawGateRun {
    name: String,
    argv: Vec<String>,
    exit_status: i32,
    output: String,
}

impl RawGateRun {
    /// Whether the command exited zero.
    fn passed(&self) -> bool {
        self.exit_status == 0
    }

    /// The clipped, event-sized record carried in the outcome.
    fn clipped(&self) -> GateCommandRun {
        GateCommandRun {
            name: self.name.clone(),
            argv: self.argv.clone(),
            exit_code: i64::from(self.exit_status),
            passed: self.passed(),
            output_tail: clip(&self.output),
        }
    }
}

/// Run a gate battery in `workspace`, in order: the ONE place gate commands
/// are executed, shared by `run_gates` and `verify_gates`.
///
/// The literal token `{base_commit}` in EVERY argv element is replaced with
/// the run's recorded base commit before execution (§ the `{base_commit}`
/// gate token) — so a brief pins its scope fence to the provisioned base
/// without the operator re-typing a SHA per dispatch; a brief with a literal
/// SHA is unaffected (no token to replace). The RESOLVED argv is recorded on
/// each run so the evidence shows the real command. An empty argv can never
/// run and can never be a verdict: a configuration fault, surfaced loudly. A
/// non-zero exit is recorded DATA the caller interprets; only an unrunnable
/// command (or a missing workspace) is terminal.
fn run_gate_battery(
    shell: &Shell,
    workspace: &str,
    base_commit: &str,
    gates: &[GateCommand],
) -> Result<Vec<RawGateRun>, ActivityFailure> {
    let mut raw: Vec<RawGateRun> = Vec::with_capacity(gates.len());
    for gate in gates {
        let resolved: Vec<String> = gate
            .argv
            .iter()
            .map(|element| element.replace("{base_commit}", base_commit))
            .collect();
        let Some((executable, rest)) = resolved.split_first() else {
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
        raw.push(RawGateRun {
            name: gate.name.clone(),
            argv: resolved,
            exit_status: run.exit_status,
            output: run.output,
        });
    }
    Ok(raw)
}

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

    let raw = run_gate_battery(shell, workspace, &base_commit, &gates)?;
    let mut runs: Vec<GateCommandRun> = Vec::with_capacity(raw.len());
    let mut diagnostics: Vec<String> = Vec::new();
    for record in &raw {
        if !record.passed() {
            diagnostics.push(format!(
                "gate `{}` exited {}:\n{}",
                record.name,
                record.exit_status,
                clip(record.output.trim())
            ));
        }
        runs.push(record.clipped());
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

    // Before the `git worktree remove --force` below can touch anything,
    // PROVE the target is strictly under the repo's dev-brief worktree root —
    // the repository itself must be unreachable by this destructive path even
    // under a misconfigured input.
    paths::guard_destructive_path(&repo_root, &workspace_path)?;

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

// --- reset_workspace -------------------------------------------------------------------

/// `reset_workspace`: the mechanical post-review restore, run after EVERY lens
/// round. `git clean -fd` removes untracked droppings; `git checkout -- .`
/// reverts tracked edits — together re-establishing the worktree at the
/// reviewed branch head before the next developer round or the verify stage.
///
/// `-fd`, deliberately NOT `-fdx`: git-ignored build products (`target/`) are
/// expensive to rebuild and belong to no lens, so they SURVIVE the reset —
/// only working-tree droppings are removed.
///
/// The lenses run only on a green gate, so the tree is provably clean when
/// they start: a non-empty `git status --porcelain` here means a lens WROTE
/// into the shared worktree despite the reviewer tool deny-list. That is
/// recorded as `was_clean: false` with the droppings — evidence for the
/// operator, never a failed run.
///
/// # Errors
///
/// Terminal when the destructive-path guard refuses the target, or when `git`
/// cannot run as infrastructure.
pub fn reset(shell: &Shell, input: ResetInput) -> Result<ResetOutcome, ActivityFailure> {
    let ResetInput {
        repo_root,
        workspace_path,
    } = input;
    // PROVE the target is strictly under the repo's dev-brief worktree root
    // before `git clean -fd`/`git checkout` can delete or overwrite anything.
    paths::guard_destructive_path(&repo_root, &workspace_path)?;

    // Capture the droppings BEFORE the restore: this listing IS the evidence a
    // lens misbehaved. Normally empty.
    let status = require_run(
        shell,
        "git",
        &["-C", &workspace_path, "status", "--porcelain"],
        &workspace_path,
        "git status (pre-reset droppings)",
    )?;
    let droppings: Vec<String> = status
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    let was_clean = droppings.is_empty();

    require_run(
        shell,
        "git",
        &["-C", &workspace_path, "clean", "-fd"],
        &workspace_path,
        "git clean -fd (post-review reset)",
    )?;
    require_run(
        shell,
        "git",
        &["-C", &workspace_path, "checkout", "--", "."],
        &workspace_path,
        "git checkout -- . (post-review reset)",
    )?;

    let detail = if was_clean {
        "worktree already clean; the post-review reset was a no-op".to_owned()
    } else {
        format!(
            "post-review reset restored the worktree; a lens had left {} \
             working-tree change(s):\n{}",
            droppings.len(),
            clip(status.output.trim())
        )
    };
    Ok(ResetOutcome {
        was_clean,
        droppings,
        detail,
    })
}

// --- verify_gates ----------------------------------------------------------------------

/// `verify_gates`: the post-accept verification battery, re-run in the SAME
/// workspace before cleanup. Recorded EVIDENCE only — a red gate never loops
/// the developer back and never changes the disposition (the workflow attaches
/// this outcome to `BriefResult.verification`).
///
/// The handler OWNS the clean-tree assertion (`git status --porcelain` empty)
/// so it can never be forgotten as an operator-authored gate: a clean tree
/// means the directory IS the branch head, so the gates test exactly what
/// merges. It is asserted before AND after the battery — there is no
/// normalization commit in the verify stage (fmt is already normalized by this
/// point), so a gate that DIRTIES the tree is itself a recorded failure
/// (`post_clean: false`).
///
/// The FULL untruncated per-command logs are written to `log_path`
/// (`<repo_root>/.yggdrasil-worktrees/dev-brief/logs/<workflow-id>-verify.log`);
/// the event payload keeps the same clipped output as `run_gates`, plus the
/// log path.
///
/// # Errors
///
/// Terminal when a gate cannot RUN, when `git` fails as infrastructure, or
/// when the log file (or its directory) cannot be written.
pub fn verify_gates(shell: &Shell, input: VerifyInput) -> Result<VerifyOutcome, ActivityFailure> {
    let VerifyInput {
        workspace_path,
        base_commit,
        gates,
        log_path,
    } = input;
    let workspace = &workspace_path;

    let pre_status = require_run(
        shell,
        "git",
        &["-C", workspace, "status", "--porcelain"],
        workspace,
        "git status (verify pre-clean assertion)",
    )?;
    let pre_clean = pre_status.stdout.trim().is_empty();

    let raw = run_gate_battery(shell, workspace, &base_commit, &gates)?;

    let post_status = require_run(
        shell,
        "git",
        &["-C", workspace, "status", "--porcelain"],
        workspace,
        "git status (verify post-clean check)",
    )?;
    let post_clean = post_status.stdout.trim().is_empty();

    write_verify_log(
        &log_path,
        &raw,
        pre_clean,
        post_clean,
        pre_status.output.trim(),
        post_status.output.trim(),
    )?;

    let runs: Vec<GateCommandRun> = raw.iter().map(RawGateRun::clipped).collect();
    let all_green = runs.iter().all(|run| run.passed);
    let pass = all_green && pre_clean && post_clean;
    let detail = verify_detail(all_green, pre_clean, post_clean, runs.len());
    Ok(VerifyOutcome {
        pass,
        pre_clean,
        post_clean,
        runs,
        log_path,
        detail,
    })
}

/// Write the untruncated verification log: the clean-tree assertions and every
/// gate's full argv + output. Creating the logs directory and the write are
/// both terminal on failure — a verify stage that could not record its
/// evidence must surface loudly, not silently drop the log.
fn write_verify_log(
    log_path: &str,
    raw: &[RawGateRun],
    pre_clean: bool,
    post_clean: bool,
    pre_status: &str,
    post_status: &str,
) -> Result<(), ActivityFailure> {
    if let Some(parent) = Path::new(log_path).parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ActivityFailure::terminal(format!(
                "could not create the verify log directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let mut sections: Vec<String> = vec![format!(
        "# verify_gates log\n\npre_clean: {pre_clean}\npost_clean: {post_clean}\n\n"
    )];
    if !pre_clean {
        sections.push(format!(
            "## pre-run git status --porcelain (tree was NOT clean going in)\n\n{pre_status}\n\n"
        ));
    }
    for record in raw {
        sections.push(format!(
            "## gate `{}` (exit {})\nargv: {:?}\n\n{}\n\n",
            record.name, record.exit_status, record.argv, record.output
        ));
    }
    if !post_clean {
        sections.push(format!(
            "## post-run git status --porcelain (a verify gate DIRTIED the tree)\n\n{post_status}\n\n"
        ));
    }
    std::fs::write(log_path, sections.concat()).map_err(|error| {
        ActivityFailure::terminal(format!(
            "could not write the verify log {log_path}: {error}"
        ))
    })
}

/// The verification's human-readable verdict line for the outcome `detail`.
fn verify_detail(all_green: bool, pre_clean: bool, post_clean: bool, ran: usize) -> String {
    if !pre_clean {
        return "verify stage ran on a DIRTY tree (git status was not empty) — the \
                directory did not equal the branch head, so these gates did not test \
                exactly what merges"
            .to_owned();
    }
    if !post_clean {
        return "a verify gate DIRTIED the tree (there is no normalization commit in \
                the verify stage) — recorded as a verification failure"
            .to_owned();
    }
    if all_green {
        format!("all {ran} verify gate(s) green on a clean tree")
    } else {
        format!(
            "{ran} verify gate(s) ran on a clean tree; at least one was red — recorded evidence"
        )
    }
}

// --- helpers --------------------------------------------------------------------------

/// Clip captured output to [`CLIP_LIMIT`], keeping the head and tail around
/// an explicit marker so a cut is never silent. The marker states the capture
/// is TRUNCATED (not the complete text), so a reader — a review lens included
/// — can distinguish "elided here" from "nothing was there" and, for a diff,
/// reconstruct the full text with `git diff <base_commit>` in the workspace.
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
        "{}\n[... {} bytes TRUNCATED — this is a partial capture, not the complete \
         text; for a diff, run `git diff <base_commit>` in the workspace for the \
         full change ...]\n{}",
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
