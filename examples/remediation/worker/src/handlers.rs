//! The five SHELL activity handler bodies: `provision_workspace`, `gate1`,
//! `gate2`, `ledger_update`, `cleanup_workspace`. Each shells to real
//! `git`/`cargo`/`python3` through [`crate::shell::Shell`].
//!
//! Failure taxonomy (the pipeline-run discipline): a command that cannot RUN
//! (missing executable, dead working directory) is INFRASTRUCTURE failure — a
//! terminal [`ActivityFailure`], because retrying a broken environment cannot
//! help. A command whose non-zero exit is a CONTRACT verdict (an authored
//! test that should fail, a red clippy, a refused ledger transition) is
//! RECORDED DATA returned as a successful activity result — never an error,
//! so the exit status lands in durable workflow history. Nothing is ever
//! swallowed into a success: every red check rides back with its captured
//! output.
//!
//! The bodies are plain synchronous `(&Shell, Input) -> Result<Output, _>` so
//! the hermetic tests drive them directly with fake-CLI shims on a private
//! `PATH`; `main.rs` adapts them onto the worker's async handler signature.

use std::path::Path;

use aion_worker::ActivityFailure;

use crate::shell::{CliRun, Shell};
use crate::types::{
    CleanupInput, CleanupOutcome, Gate1Input, Gate1Outcome, Gate2Checks, Gate2Input, Gate2Outcome,
    LedgerUpdateInput, LedgerUpdateOutcome, ProvisionInput, TestRun, WorkspaceInfo,
};

/// The base directory brief worktrees live under. MUST match
/// `remediation_brief.workspace_base` in the Gleam workflow, because the
/// child derives each brief's `workspace_path` from it and the driven
/// harnesses point Norn's `--workspace-root` at the same
/// `<base>/{workflow_id}`.
pub const WORKSPACE_BASE: &str = "/tmp/aion-remediation/ws";

/// The applier CLI, relative to the target repo root (built in the yggdrasil
/// repo; the contract is recorded in this example's README).
pub const APPLY_TRANSITIONS: &str = "scripts/remediation/apply_transitions.py";

/// Evidence clip bound: capture is kept whole below this, else head + tail
/// with an explicit truncation marker (durable history should carry the
/// verdict's shape, not megabytes of cargo spew; the marker keeps the cut
/// honest).
const CLIP_LIMIT: usize = 16_384;

// --- provision_workspace ------------------------------------------------------

/// `provision_workspace`: create the brief's isolated git worktree at
/// `workspace_path`, checking out `branch` freshly based on `base_branch`,
/// and report the base commit gate 1 will diff against.
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

// --- gate1 ----------------------------------------------------------------------

/// `gate1`: the fail-first gate, FULLY MECHANICAL (DESIGN.md Gate 1,
/// 2026-07-07 contract — evidence is verified, not trusted; no judgment
/// left):
///
/// 1. The authored tests must be COMMITTED (the test-author's contract, and
///    load-bearing: gate 2's tamper diff is taken against this commit). A
///    dirty worktree is a recorded gate failure.
/// 2. The authored set is pinned mechanically (files changed since the
///    provisioned base commit) and SCOPE-CHECKED: every changed path must be
///    a manifest `test_file` or match the shared test-path rule
///    ([`is_test_path`]) — a test-author diff touching production code is a
///    gate failure.
/// 3. Per runnable check, every named test is re-run; each must (a) FAIL
///    (non-zero cargo exit) and (b) print the check's
///    `expected_failure_signature` — failing for the RIGHT reason. A passing
///    test, an unmatched name, or a missing signature is a recorded gate
///    failure with the captured output as evidence.
/// 4. AFTER the fail-first re-runs (so a wrong-reason failure stays the
///    primary signal when both are broken), each DISTINCT crate containing
///    an authored file must pass
///    `cargo clippy -p <package> --all-targets -- -D warnings` (Scott's
///    paradox ruling, DESIGN.md 1b1cf0c0): gate 2 lints the whole workspace
///    with the authored tests FROZEN, so a lint shipped in a test file would
///    otherwise turn gate 2 into a gate the developer cannot pass. The
///    package name is derived mechanically — walk up from the authored file
///    to the nearest `Cargo.toml` and read `[package].name`. A violation is
///    a recorded gate failure with the clippy output as evidence; zero
///    authored files (or none inside a package) skips the check.
/// 5. Manual-acceptance entries run nothing; they are echoed into the result
///    as acceptance-check records for the verifier.
///
/// # Errors
///
/// Terminal only when `git`/`cargo` cannot run at all.
pub fn gate1(shell: &Shell, input: Gate1Input) -> Result<Gate1Outcome, ActivityFailure> {
    let Gate1Input {
        workspace_path,
        base_commit,
        checks,
        acceptance,
        test_files,
    } = input;
    let workspace = &workspace_path;

    let status = require_run(
        shell,
        "git",
        &["-C", workspace, "status", "--porcelain"],
        workspace,
        "git status (authored tests committed?)",
    )?;

    let head = require_run(
        shell,
        "git",
        &["-C", workspace, "rev-parse", "HEAD"],
        workspace,
        "git rev-parse HEAD (tests commit)",
    )?;
    let tests_commit = head.stdout.trim().to_owned();

    let changed = require_run(
        shell,
        "git",
        &[
            "-C",
            workspace,
            "diff",
            "--name-only",
            &format!("{base_commit}..HEAD"),
        ],
        workspace,
        "git diff --name-only (authored test set)",
    )?;
    let authored_test_paths: Vec<String> = changed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();

    // (d) Diff scope: every authored path must be an explicitly-declared
    // test_file or match the shared test-path rule. Production code touched
    // by the test author is a gate failure, per path.
    let scope_violations: Vec<String> = authored_test_paths
        .iter()
        .filter(|path| !test_files.contains(path) && !is_test_path(path))
        .cloned()
        .collect();

    if !status.stdout.trim().is_empty() {
        // Uncommitted tests break the whole tamper mechanism downstream, and
        // violate the test-author's commit contract: a recorded gate failure,
        // not an auto-commit that would mask it.
        return Ok(Gate1Outcome {
            pass: false,
            results: Vec::new(),
            acceptance_checks: acceptance,
            scope_violations,
            authored_test_paths,
            tests_commit,
            detail: format!(
                "authored tests are not committed; gate 1 requires the test \
                 author's work committed on the brief branch. git status:\n{}",
                clip(&status.output)
            ),
        });
    }

    let (results, mut failures) = rerun_authored_checks(shell, workspace, &checks)?;
    if !scope_violations.is_empty() {
        failures.push(format!(
            "test-author diff touches non-test paths (forbidden): {}",
            scope_violations.join(", ")
        ));
    }
    // Clippy on the authored crates — deliberately AFTER the fail-first
    // re-runs, so its line lands after any wrong-reason failure in `detail`.
    if let Some(clippy_failure) = clippy_authored_crates(shell, workspace, &authored_test_paths)? {
        failures.push(clippy_failure);
    }

    let pass = failures.is_empty();
    let ran = results.len();
    Ok(Gate1Outcome {
        pass,
        results,
        acceptance_checks: acceptance,
        scope_violations,
        authored_test_paths,
        tests_commit,
        detail: if pass {
            if ran == 0 {
                "no authored tests to re-run (all entries could_not_reproduce \
                 or manual_acceptance); authored set pinned for gate 2"
                    .to_owned()
            } else {
                format!(
                    "all {ran} authored test(s) failed on the unfixed code \
                     with their expected signatures, as required"
                )
            }
        } else {
            failures.join("; ")
        },
    })
}

/// Re-run every authored test of every check; each MUST fail (non-zero cargo
/// exit — the test encodes a live defect) AND print the check's expected
/// failure signature (failing for the RIGHT reason — a compile error or an
/// unrelated panic fails without the signature and is caught here
/// mechanically). Returns the per-test records plus a line per violation.
///
/// # Errors
///
/// Terminal when `cargo` cannot run at all.
fn rerun_authored_checks(
    shell: &Shell,
    workspace: &str,
    checks: &[crate::types::Gate1Check],
) -> Result<(Vec<TestRun>, Vec<String>), ActivityFailure> {
    let mut results = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for check in checks {
        for test_name in &check.test_names {
            let run = require_can_run(
                shell,
                "cargo",
                &["test", "--workspace", test_name],
                workspace,
                "cargo test (authored fail-first re-run)",
            )?;
            let failed = !run.succeeded();
            let signature_matched = run.output.contains(&check.expected_failure_signature);
            if !failed {
                failures.push(format!(
                    "{}: `{test_name}` did not fail on the unfixed code (exit 0{})",
                    check.finding_id,
                    if ran_any_tests(&run.output) {
                        ""
                    } else {
                        "; no test matched the name"
                    }
                ));
            } else if !signature_matched {
                failures.push(format!(
                    "{}: `{test_name}` failed WITHOUT the expected failure \
                     signature `{}` in its output — failing for the wrong reason",
                    check.finding_id, check.expected_failure_signature
                ));
            }
            results.push(TestRun {
                finding_id: check.finding_id.clone(),
                test_name: test_name.clone(),
                failed,
                signature_matched,
                evidence: clip(&run.output),
            });
        }
    }
    Ok((results, failures))
}

/// Run `cargo clippy -p <package> --all-targets -- -D warnings` in the
/// workspace root for each DISTINCT crate containing an authored file. A
/// non-zero exit is the returned gate-failure line — naming the crate, with
/// the clipped clippy output as evidence — and stops the sweep (first
/// failure wins). `None` means every authored crate is clippy-clean, or no
/// authored file resolved to a package (nothing to check).
///
/// # Errors
///
/// Terminal when `cargo` cannot run at all.
fn clippy_authored_crates(
    shell: &Shell,
    workspace: &str,
    authored_test_paths: &[String],
) -> Result<Option<String>, ActivityFailure> {
    for package in authored_packages(workspace, authored_test_paths) {
        let run = require_can_run(
            shell,
            "cargo",
            &[
                "clippy",
                "-p",
                &package,
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            workspace,
            "cargo clippy (authored test targets)",
        )?;
        if !run.succeeded() {
            return Ok(Some(format!(
                "authored tests in crate `{package}` fail `cargo clippy -D \
                 warnings` — the developer is forbidden to modify them, so \
                 gate 2's workspace-wide clippy would be unpassable; the \
                 author must ship them lint-clean:\n{}",
                clip(&run.output)
            )));
        }
    }
    Ok(None)
}

/// The DISTINCT `[package].name`s of the crates containing the authored
/// files, in first-seen order. Authored paths that resolve to no package
/// (no package-bearing `Cargo.toml` above them inside the workspace)
/// contribute nothing.
fn authored_packages(workspace: &str, authored_test_paths: &[String]) -> Vec<String> {
    let root = Path::new(workspace);
    let mut packages: Vec<String> = Vec::new();
    for path in authored_test_paths {
        let Some(package) = package_of(root, Path::new(path)) else {
            continue;
        };
        if !packages.contains(&package) {
            packages.push(package);
        }
    }
    packages
}

/// The `[package].name` of the nearest `Cargo.toml` at or above `file`
/// (a workspace-root-relative path), walking up to the workspace root and
/// never past it. `None` when no manifest is found or the nearest one
/// declares no package (a virtual workspace manifest).
fn package_of(root: &Path, file: &Path) -> Option<String> {
    let mut dir = root.join(file);
    loop {
        dir = dir.parent()?.to_path_buf();
        if !dir.starts_with(root) {
            return None;
        }
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            return package_name(&std::fs::read_to_string(manifest).ok()?);
        }
        if dir == root {
            return None;
        }
    }
}

/// Extract `name = "…"` from a manifest's `[package]` section. Deliberately
/// plain line parsing — the same no-external-deps discipline as the rest of
/// this worker; a manifest odd enough to defeat it simply yields `None` and
/// the crate goes unchecked here (gate 2 still covers the workspace).
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = line.strip_prefix("name") else {
            continue;
        };
        if let Some(value) = rest.trim_start().strip_prefix('=') {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

/// The shared test-path rule (used by gate 1's diff-scope check; gate 2's
/// tamper protection covers the full authored set gate 1 admitted under this
/// rule): a path is a test path iff some directory component is `tests` or
/// `test`, or its file stem ends in `_test`/`_tests`, or its file name
/// starts with `test_`. Manifest `test_file` paths are additionally allowed
/// explicitly by the caller.
#[must_use]
pub fn is_test_path(path: &str) -> bool {
    let component_is_test_dir = path
        .split('/')
        .rev()
        .skip(1)
        .any(|component| component == "tests" || component == "test");
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    component_is_test_dir
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || file_name.starts_with("test_")
}

/// Whether the cargo output shows at least one test actually executed
/// (`running N tests` with N > 0 in any test binary).
fn ran_any_tests(output: &str) -> bool {
    output.lines().any(|line| {
        line.strip_prefix("running ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|count| count.parse::<u64>().ok())
            .is_some_and(|count| count > 0)
    })
}

// --- gate2 -----------------------------------------------------------------------

/// `gate2`: the mechanical fix gate (DESIGN.md Gate 2).
///
/// - TAMPER: the diff over the authored test paths against the gate-1 commit
///   must be empty. The diff is taken worktree-vs-commit, so committed AND
///   uncommitted edits are both caught.
/// - `cargo fmt` (write mode, never a check) then
///   `cargo clippy --workspace --all-targets -- -D warnings` then
///   `cargo test --workspace`; all three checks always run so a loop-back
///   carries the full picture.
/// - The full diff (worktree vs the gate-1 commit) is captured for the
///   verifier.
///
/// # Errors
///
/// Terminal only when `git`/`cargo` cannot run at all.
pub fn gate2(shell: &Shell, input: Gate2Input) -> Result<Gate2Outcome, ActivityFailure> {
    let Gate2Input {
        workspace_path,
        tests_commit,
        authored_test_paths,
    } = input;
    let workspace = &workspace_path;
    let mut diagnostics: Vec<String> = Vec::new();

    let test_diff_clean = if authored_test_paths.is_empty() {
        true
    } else {
        let mut args = vec!["-C", workspace, "diff", "--name-only", &tests_commit, "--"];
        args.extend(authored_test_paths.iter().map(String::as_str));
        let tamper = require_run(
            shell,
            "git",
            &args,
            workspace,
            "git diff --name-only (authored-test tamper check)",
        )?;
        let touched: Vec<&str> = tamper
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        if touched.is_empty() {
            true
        } else {
            diagnostics.push(format!(
                "authored tests were modified (forbidden — they are the \
                 contract): {}",
                touched.join(", ")
            ));
            false
        }
    };

    // Autoformat first (write mode, never a check) — a formatting-only
    // difference must not fail the gate, so its outcome is ignored.
    let _ = shell.run("cargo", &["fmt", "--all"], workspace);

    let clippy = require_can_run(
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
        workspace,
        "cargo clippy",
    )?;
    let clippy_pass = clippy.succeeded();
    if !clippy_pass {
        diagnostics.push(format!(
            "cargo clippy -D warnings failed:\n{}",
            clip(&clippy.output)
        ));
    }

    let suite = require_can_run(
        shell,
        "cargo",
        &["test", "--workspace"],
        workspace,
        "cargo test",
    )?;
    let suite_pass = suite.succeeded();
    if !suite_pass {
        diagnostics.push(format!("cargo test failed:\n{}", clip(&suite.output)));
    }

    // The developer's full change for the verifier: worktree vs the gate-1
    // commit, so uncommitted work is visible too.
    let diff = require_run(
        shell,
        "git",
        &["-C", workspace, "diff", &tests_commit],
        workspace,
        "git diff (verifier evidence)",
    )?;

    Ok(Gate2Outcome {
        pass: test_diff_clean && clippy_pass && suite_pass,
        checks: Gate2Checks {
            test_diff_clean,
            clippy_pass,
            suite_pass,
        },
        diagnostics: diagnostics.join("\n\n"),
        diff: clip(&diff.stdout),
    })
}

// --- ledger_update ------------------------------------------------------------------

/// `ledger_update`: apply one stage artifact to the in-repo ledger via
/// `python3 scripts/remediation/apply_transitions.py --ledger <path>
/// --artifact <artifact.json> --kind <kind>`, run in `repo_root`.
///
/// The artifact JSON is materialized to a temp file for the CLI. A non-zero
/// applier exit is `applied: false` with the captured output — recorded, so
/// the workflow carries the refusal on the brief result; a status must never
/// change without its artifact, and an unapplied artifact must never look
/// applied.
///
/// # Errors
///
/// Terminal when `python3` cannot run at all or the artifact cannot be
/// written to disk.
pub fn ledger_update(
    shell: &Shell,
    input: LedgerUpdateInput,
) -> Result<LedgerUpdateOutcome, ActivityFailure> {
    let LedgerUpdateInput {
        repo_root,
        ledger_path,
        kind,
        artifact_json,
    } = input;
    let artifact_file = tempfile::Builder::new()
        .prefix("remediation-artifact-")
        .suffix(".json")
        .tempfile()
        .map_err(|error| {
            ActivityFailure::terminal(format!("could not create the artifact temp file: {error}"))
        })?;
    std::fs::write(artifact_file.path(), artifact_json.as_bytes()).map_err(|error| {
        ActivityFailure::terminal(format!("could not write the artifact temp file: {error}"))
    })?;
    let artifact_path = artifact_file.path().display().to_string();

    let run = require_can_run(
        shell,
        "python3",
        &[
            APPLY_TRANSITIONS,
            "--ledger",
            &ledger_path,
            "--artifact",
            &artifact_path,
            "--kind",
            kind.as_str(),
        ],
        &repo_root,
        "apply_transitions.py",
    )?;

    Ok(LedgerUpdateOutcome {
        applied: run.succeeded(),
        detail: if run.succeeded() {
            format!(
                "applied {} artifact: {}",
                kind.as_str(),
                clip(run.output.trim())
            )
        } else {
            format!(
                "applier exited {} for {} artifact: {}",
                run.exit_status,
                kind.as_str(),
                clip(run.output.trim())
            )
        },
    })
}

// --- cleanup_workspace -----------------------------------------------------------------

/// `cleanup_workspace`: remove the brief's worktree. The branch (and every
/// commit on it) remains in the repository.
///
/// A DIRTY worktree is never removed — `git worktree remove --force` would
/// destroy uncommitted work, the exact defect class (unguarded teardown
/// deletion, YG-268/367) Wave 0 exists to fix. It is left in place and
/// reported.
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
