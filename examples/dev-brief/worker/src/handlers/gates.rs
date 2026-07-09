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
