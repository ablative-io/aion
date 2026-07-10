//! The gate-battery core and the `run_gates` handler. The battery core
//! ([`run_gate_battery`]) is the ONE place gate commands execute — shared by
//! `run_gates` here and `verify_gates` in [`super::verify`] — so the
//! `{base_commit}` token substitution and the resolved-argv recording can
//! never diverge between the two stages.

use aion_worker::ActivityFailure;

use crate::commit;
use crate::shell::Shell;
use crate::types::{GateCommand, GateCommandRun, GateInput, GateOutcome};

use super::support::{clip, require_can_run, require_run};

/// One gate command's RAW (untruncated) execution record: the resolved argv,
/// the exit status, and the full combined output. `run_gates` clips this into
/// the event's [`GateCommandRun`]; `verify_gates` additionally writes the full
/// output to its log file before clipping.
pub(super) struct RawGateRun {
    pub(super) name: String,
    pub(super) argv: Vec<String>,
    pub(super) exit_status: i32,
    pub(super) output: String,
}

impl RawGateRun {
    /// Whether the command exited zero.
    pub(super) fn passed(&self) -> bool {
        self.exit_status == 0
    }

    /// The clipped, event-sized record carried in the outcome.
    pub(super) fn clipped(&self) -> GateCommandRun {
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
pub(super) fn run_gate_battery(
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

/// Whether the workspace tree is clean (`git status --porcelain` empty). Used
/// to bound the post-normalization re-run to a single pass: a gate that
/// re-dirties the tree is a recorded failure, not a re-commit-and-re-run loop.
fn worktree_clean(shell: &Shell, workspace: &str) -> Result<bool, ActivityFailure> {
    let status = require_run(
        shell,
        "git",
        &["-C", workspace, "status", "--porcelain"],
        workspace,
        "git status (post-normalization clean assertion)",
    )?;
    Ok(status.stdout.trim().is_empty())
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
/// 3. When that commit moved HEAD, the FULL battery is re-run against the
///    post-normalization HEAD and THOSE statuses become the recorded verdict —
///    the normalization commit can flip a scope fence (or any HEAD-sensitive
///    gate) red, and the reviewed commit must never be recorded green on a
///    verdict it never earned. The re-run is bounded to a single pass: a gate
///    that re-dirties the tree is itself a recorded failure.
/// 4. The reviewers' diff is captured as worktree-vs-`base_commit`, so
///    committed AND (any residual) uncommitted work are both visible.
/// 5. An EMPTY battery is the operator's explicit choice: a recorded vacuous
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

    // Judge the developer's HEAD.
    let raw = run_gate_battery(shell, workspace, &base_commit, &gates)?;

    // Gate commands may normalize the tree (cargo fmt in write mode is the
    // recommended first gate). That change belongs on the branch: commit it
    // mechanically so the branch is complete and the final cleanup sees a
    // clean worktree. Nothing dirty skips green.
    let normalization =
        commit::commit_gate_normalization(shell, workspace).map_err(ActivityFailure::terminal)?;

    // The recorded verdict must be authoritative for the REVIEWED HEAD. The
    // normalization commit moved HEAD to a commit the step-above battery never
    // saw — a write-mode formatter can touch files OUTSIDE the brief's scope,
    // flipping a scope fence pinned to `{base_commit}` red at the very commit
    // the reviewers see. So when the normalization committed, re-run the FULL
    // battery against the post-normalization HEAD and let THAT verdict stand:
    // a green the reviewed commit never earned must never reach review. A
    // Skipped normalization leaves the reviewed HEAD == the judged HEAD, so no
    // re-run is owed and the fast path stays cheap.
    let mut normalization_notes: Vec<String> = Vec::new();
    let mut redirty_note: Option<String> = None;
    let raw = match &normalization {
        commit::FixCommitOutcome::Committed { commit, paths } => {
            normalization_notes.push(format!(
                "gate commands normalized the tree; committed mechanically as \
                 {commit} ({} path(s))",
                paths.len()
            ));
            let pre_green = raw.iter().all(RawGateRun::passed);
            let rerun = run_gate_battery(shell, workspace, &base_commit, &gates)?;
            // Mirror verify_gates' clean-tree assertion to bound this to a
            // SINGLE re-run: a gate that re-dirties the tree on the re-run (a
            // non-idempotent formatter) is a recorded failure, not an unbounded
            // normalize→re-run loop — the reviewed commit does not embody those
            // uncommitted changes, so the tree does not equal the branch head.
            if !worktree_clean(shell, workspace)? {
                redirty_note = Some(
                    "a gate command re-dirtied the worktree on the post-normalization \
                     re-run (a non-idempotent formatter): the reviewed commit does not \
                     embody these changes — recorded as a gate failure, looping back to \
                     the developer"
                        .to_owned(),
                );
            }
            if pre_green && !rerun.iter().all(RawGateRun::passed) {
                normalization_notes.push(
                    "the post-normalization re-run flipped a gate RED — the developer's \
                     HEAD was green but the normalization commit is not; looping back to \
                     the developer with the post-normalization statuses"
                        .to_owned(),
                );
            }
            rerun
        }
        commit::FixCommitOutcome::Skipped { .. } => raw,
    };

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
    diagnostics.extend(normalization_notes);
    if let Some(note) = &redirty_note {
        diagnostics.push(note.clone());
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

    let pass = redirty_note.is_none() && runs.iter().all(|run| run.passed);
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
