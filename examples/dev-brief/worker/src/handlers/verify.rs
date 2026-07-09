//! The `verify_gates` handler: the post-accept verification stage — the
//! handler-owned clean-tree assertion, the re-run battery (through the shared
//! core in [`super::gates`]), and the untruncated log file.

use std::path::Path;

use aion_worker::ActivityFailure;

use crate::shell::Shell;
use crate::types::{GateCommandRun, VerifyInput, VerifyOutcome};

use super::gates::{RawGateRun, run_gate_battery};
use super::support::require_run;

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
