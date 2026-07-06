//! Wire types for the remediation SHELL activity payloads.
//!
//! Every type here serializes/deserializes byte-compatibly with the Gleam
//! codecs in `../../src/remediation/codecs.gleam` — those codecs are the
//! authoritative contract (field names in `snake_case`, emitted in
//! declaration order). The four DRIVEN AGENT activities need no wire types
//! here: their input is the structured context JSON the workflow encoded
//! (assembled into the prompt by [`crate::harness::ProfiledNornHarness`]) and
//! their output is produced by Norn against the embedded `--output-schema`,
//! decoded by the workflow's Gleam output codec.

use serde::{Deserialize, Serialize};

/// Input to `provision_workspace` (`codecs.provision_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionInput {
    /// The repository the worktree is created from.
    pub repo_root: String,
    /// The branch the brief branch is created on top of.
    pub base_branch: String,
    /// The branch to create and check out for this brief
    /// (`remediation/<brief-id>`).
    pub branch: String,
    /// The absolute path the worktree is created at — `<base>/<child id>`,
    /// matching the `--workspace-root` the driven harnesses give Norn.
    pub workspace_path: String,
}

/// Result of `provision_workspace` (`codecs.workspace_info_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// The worktree path the brief's activities run in.
    pub workspace_path: String,
    /// The branch checked out there.
    pub branch: String,
    /// The commit the worktree started from — gate 1 computes the
    /// authored-test paths as the files changed since it.
    pub base_commit: String,
}

/// One runnable gate-1 check (`codecs` `Gate1Check`), routed from a manifest
/// entry by the workflow: the finding's tests plus the substring their
/// failing output MUST contain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate1Check {
    /// The finding this check guards.
    pub finding_id: String,
    /// The authored test names (cargo test filters).
    pub test_names: Vec<String>,
    /// The substring the failing output must contain — the fully mechanical
    /// fails-for-the-RIGHT-reason check.
    pub expected_failure_signature: String,
}

/// One manual-acceptance entry (`codecs` `AcceptanceCheck`): an improvement/
/// completion finding with no expressible failing test. Nothing runs; the
/// criterion is echoed through the gate result for the verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCheck {
    /// The finding the criterion belongs to.
    pub finding_id: String,
    /// The observable acceptance criterion the verifier will check.
    pub criterion: String,
}

/// Input to `gate1` (`codecs.gate1_input_codec`, 2026-07-07 contract).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate1Input {
    /// The workspace the authored tests are re-run in.
    pub workspace_path: String,
    /// The provisioned base commit; files changed since it are the authored
    /// set (and must ALL be test paths).
    pub base_commit: String,
    /// The runnable checks (entries with tests, not `could_not_reproduce`).
    pub checks: Vec<Gate1Check>,
    /// The manual-acceptance entries — recorded, nothing run.
    pub acceptance: Vec<AcceptanceCheck>,
    /// The manifest's `test_file` paths — the explicitly-allowed set for the
    /// diff-scope check, alongside the shared test-path rule.
    pub test_files: Vec<String>,
}

/// One authored test's re-run (`codecs` `TestRun`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    /// The finding this run guards.
    pub finding_id: String,
    /// The test name (the cargo test filter it was run with).
    pub test_name: String,
    /// Whether the run FAILED — the required outcome at gate 1 (the test
    /// encodes a live defect).
    pub failed: bool,
    /// Whether the captured output contained the check's
    /// `expected_failure_signature` — failing for the RIGHT reason.
    pub signature_matched: bool,
    /// The captured (clipped) cargo output, whatever the outcome.
    pub evidence: String,
}

/// Result of `gate1` (`codecs.gate1_outcome_codec`). `pass` is true only when
/// the authored tests are committed, the authored diff stayed on test paths,
/// and every named test failed WITH its signature in the output. Anything
/// else is a recorded FAIL verdict with evidence, never an activity error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate1Outcome {
    /// Whether the gate passed.
    pub pass: bool,
    /// Per-test re-run results.
    pub results: Vec<TestRun>,
    /// The manual-acceptance entries, echoed for the verifier (nothing ran).
    pub acceptance_checks: Vec<AcceptanceCheck>,
    /// Authored-diff paths that are NOT test paths — production code touched
    /// by the test author (each is a gate failure).
    pub scope_violations: Vec<String>,
    /// The files changed since the base commit — the immutable authored-test
    /// set gate 2 diffs against.
    pub authored_test_paths: Vec<String>,
    /// HEAD at gate-1 time — gate 2's tamper baseline.
    pub tests_commit: String,
    /// A human-readable account of the gate's verdict.
    pub detail: String,
}

/// Input to `gate2` (`codecs.gate2_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate2Input {
    /// The workspace the fix gate runs in.
    pub workspace_path: String,
    /// The gate-1 baseline commit the authored-test tamper diff is taken
    /// against.
    pub tests_commit: String,
    /// The authored test paths that must be untouched.
    pub authored_test_paths: Vec<String>,
}

/// The three mechanical checks of gate 2, recorded independently so a
/// loop-back tells the developer exactly what was red. Serde-FLATTENED into
/// [`Gate2Outcome`], so the wire shape stays the flat object the Gleam codec
/// decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate2Checks {
    /// Whether the authored-test-path diff since `tests_commit` is empty
    /// (false = a test-edit attempt, a counted guard-failure metric).
    pub test_diff_clean: bool,
    /// Whether `cargo clippy --workspace --all-targets -- -D warnings` exited
    /// zero.
    pub clippy_pass: bool,
    /// Whether `cargo test --workspace` exited zero.
    pub suite_pass: bool,
}

/// Result of `gate2` (`codecs.gate2_outcome_codec`). A red check is recorded
/// DATA the workflow loops back on, never an activity error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate2Outcome {
    /// True only when the tamper diff is empty AND clippy AND the suite are
    /// green.
    pub pass: bool,
    /// The three checks, flattened onto the wire.
    #[serde(flatten)]
    pub checks: Gate2Checks,
    /// The combined (clipped) failure output of whichever checks were red.
    pub diagnostics: String,
    /// The developer's full change (worktree vs `tests_commit`) — the diff
    /// the verifier reads.
    pub diff: String,
}

/// The applier CLI's `--kind` vocabulary (`codecs` `ArtifactKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A Stage-1 `TestManifest`.
    TestManifest,
    /// A Stage-2 `FixReport`.
    FixReport,
    /// A Stage-3 `Verdict`.
    Verdict,
    /// The brief's terminal disposition record.
    Disposition,
}

impl ArtifactKind {
    /// The wire tag — exactly the applier CLI's `--kind` value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TestManifest => "test_manifest",
            Self::FixReport => "fix_report",
            Self::Verdict => "verdict",
            Self::Disposition => "disposition",
        }
    }
}

/// Input to `ledger_update` (`codecs.ledger_update_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerUpdateInput {
    /// The repository the applier runs in (it lives at
    /// `scripts/remediation/apply_transitions.py` there).
    pub repo_root: String,
    /// The in-repo ledger file the applier maintains.
    pub ledger_path: String,
    /// Which stage artifact this is.
    pub kind: ArtifactKind,
    /// The artifact JSON, materialized to a temp file for the CLI.
    pub artifact_json: String,
}

/// Result of `ledger_update` (`codecs.ledger_update_outcome_codec`). A
/// non-zero applier exit is `applied: false` with the captured output — the
/// workflow records it on the brief result, never swallows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerUpdateOutcome {
    /// Whether the applier accepted the artifact (exit zero).
    pub applied: bool,
    /// The captured (clipped) applier output.
    pub detail: String,
}

/// Input to `cleanup_workspace` (`codecs.cleanup_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupInput {
    /// The repository whose worktree registration is removed.
    pub repo_root: String,
    /// The worktree path to remove.
    pub workspace_path: String,
}

/// Result of `cleanup_workspace` (`codecs.cleanup_outcome_codec`). A dirty
/// worktree is refused (uncommitted work must never be destroyed) and
/// reported as `removed: false` with the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupOutcome {
    /// Whether the worktree was removed.
    pub removed: bool,
    /// Why, when it was not (dirty, absent, or the removal itself failed).
    pub detail: String,
}
