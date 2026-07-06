//// The typed data model shared across the remediation family (parent
//// `remediation_wave` and child `remediation_brief`) and the activities they
//// dispatch.
////
//// The contract is JSON-on-the-wire: every type here has a codec in
//// `remediation/codecs.gleam`. The agent-activity OUTPUT types
//// (`TestManifest`, `FixReport`, `Verdict`) are the Gleam mirror of the
//// yggdrasil remediation-flow schemas copied under `schemas/`
//// (`test-manifest.schema.json`, `fix-report.schema.json`,
//// `verdict.schema.json`) — the drift-guarded source of truth the driven
//// agents are constrained by via `--output-schema`.
////
//// INDEPENDENCE IS ENCODED IN THE TYPES: the test-author's view of a ledger
//// entry ([`TestAuthorEntry`]) has NO `recommendation` field, so the codec
//// layer is structurally incapable of leaking the recommended fix to the
//// test-author (DESIGN.md Stage 1; test-author profile hard rule 1). The
//// developer's view ([`LedgerEntry`]) carries the full entry, recommendation
//// included.

import gleam/option.{type Option}

// --- ledger entries ----------------------------------------------------------

/// A finding's category (ledger-entry.schema.json `category`). Typed, not a
/// string: the workflow's Gate-1 coverage check branches on `Correction`, and a
/// typo'd string would silently skip a finding — exactly the silent drop the
/// flow exists to prevent.
pub type Category {
  Correction
  Completion
  Improvement
}

/// The agent-facing projection of one ledger entry, FULL (recommendation
/// included) — the developer's and verifier's view. Bookkeeping fields of the
/// on-disk ledger (`status`, `status_history`, join metadata, ...) are not
/// carried: they are the applier's domain, and the agents' declared inputs are
/// exactly these fields (see the role profiles).
pub type LedgerEntry {
  LedgerEntry(
    id: String,
    title: String,
    file: String,
    line: Int,
    category: Category,
    severity: String,
    detail: String,
    failure_scenario: String,
    recommendation: String,
  )
}

/// The TEST-AUTHOR's view of a ledger entry: the same fields as
/// [`LedgerEntry`] with `recommendation` structurally absent. Built only via
/// [`strip_recommendation`]; its codec cannot emit a `recommendation` key
/// because the type has no such field to encode.
pub type TestAuthorEntry {
  TestAuthorEntry(
    id: String,
    title: String,
    file: String,
    line: Int,
    category: Category,
    severity: String,
    detail: String,
    failure_scenario: String,
  )
}

/// Project a full ledger entry down to the test-author's recommendation-free
/// view. The ONLY constructor path for [`TestAuthorEntry`] values in the
/// workflow.
pub fn strip_recommendation(entry: LedgerEntry) -> TestAuthorEntry {
  TestAuthorEntry(
    id: entry.id,
    title: entry.title,
    file: entry.file,
    line: entry.line,
    category: entry.category,
    severity: entry.severity,
    detail: entry.detail,
    failure_scenario: entry.failure_scenario,
  )
}

// --- brief (Stage 0 output, workflow input) ---------------------------------

/// A fix brief per `brief.schema.json`: findings clustered by shared root
/// cause and file locality. `files_expected`/`boundaries` flatten the schema's
/// nested `scope` object; the codec preserves the schema's wire shape.
pub type Brief {
  Brief(
    id: String,
    finding_ids: List(String),
    root_cause: String,
    files_expected: List(String),
    boundaries: List(String),
    acceptance: List(String),
    wave: Int,
    deep_cluster: Bool,
  )
}

// --- run configuration -------------------------------------------------------

/// Operational inputs carried alongside the briefs: the repository the fixes
/// land in, the in-repo ledger the applier maintains (DECISIONS.md D1), the
/// branch the per-brief worktrees are based on, and the fix-cycle budget.
/// `base_branch` and `max_fix_cycles` are overridable process defaults, never
/// hidden constants: absent from the input, they resolve to
/// [`default_base_branch`] / [`default_max_fix_cycles`].
pub type RunConfig {
  RunConfig(
    repo_root: String,
    ledger_path: String,
    base_branch: String,
    max_fix_cycles: Int,
  )
}

/// The default branch per-brief worktrees are based on when the input omits
/// `base_branch`.
pub fn default_base_branch() -> String {
  "main"
}

/// The default developer-round budget (initial fix + loop-backs) when the
/// input omits `max_fix_cycles` (task contract: cap in input, default 3).
pub fn default_max_fix_cycles() -> Int {
  3
}

// --- child (remediation_brief) input -----------------------------------------

/// Input to the child `remediation_brief` workflow: one brief, its member
/// ledger entries (full — the codec strips the recommendation only for the
/// test-author's activity input), and the run configuration.
pub type BriefInput {
  BriefInput(brief: Brief, entries: List(LedgerEntry), config: RunConfig)
}

// --- agent activity inputs ---------------------------------------------------

/// Input to the `test_author` agent activity: the brief plus the
/// RECOMMENDATION-FREE entry projections. The recommendation is stripped at
/// the codec layer — this type cannot carry it, so it can never reach the
/// wire.
pub type TestAuthorInput {
  TestAuthorInput(brief: Brief, entries: List(TestAuthorEntry))
}

/// Input to the `developer` agent activity: the brief, the FULL ledger entries
/// (recommendation included — the developer may use it as a starting point),
/// the authored-test manifest, the gate-1 re-run evidence, and — on loop-back
/// rounds — the adverse verdict and/or failing gate-2 outcome being addressed.
pub type DeveloperInput {
  DeveloperInput(
    brief: Brief,
    entries: List(LedgerEntry),
    manifest: TestManifest,
    gate1_results: List(TestRun),
    verdict: Option(Verdict),
    gate2: Option(Gate2Outcome),
  )
}

/// Input to the `verifier` agent activity: the original findings, the
/// developer's diff, the fix report, and the test manifest (DESIGN.md
/// Stage 3's declared inputs, verbatim).
pub type VerifierInput {
  VerifierInput(
    brief: Brief,
    entries: List(LedgerEntry),
    manifest: TestManifest,
    fix_report: FixReport,
    diff: String,
  )
}

// --- agent activity outputs (schema mirrors) ---------------------------------

/// One test-manifest entry per `test-manifest.schema.json` (2026-07-07
/// contract). `test_file` and `expected_failure_signature` make gate 1 fully
/// mechanical: re-run, assert failure, assert the output contains the
/// signature, assert the authored diff stays on test paths.
/// `could_not_reproduce: True` routes the finding to the operator (Wave 0,
/// DECISIONS.md D4 — no automated reroute), carried through to the brief
/// result. `manual_acceptance` is the channel for improvement/completion
/// findings with no expressible failing test: gate 1 records the criterion
/// for the verifier instead of running anything.
pub type ManifestEntry {
  ManifestEntry(
    finding_id: String,
    test_names: List(String),
    test_file: String,
    expected_failure_signature: String,
    fail_evidence: String,
    could_not_reproduce: Bool,
    could_not_reproduce_reason: Option(String),
    manual_acceptance: Option(String),
  )
}

/// The test-author's structured result per `test-manifest.schema.json`.
pub type TestManifest {
  TestManifest(brief_id: String, entries: List(ManifestEntry))
}

/// One addressed finding in a fix report (`fix-report.schema.json`).
pub type FindingFix {
  FindingFix(finding_id: String, how: String)
}

/// One explicitly bounced finding in a fix report (`fix-report.schema.json`
/// `findings_bounced`): the developer's concrete evidence the finding is
/// invalid. Distinct from addressed — gate 2's accounting requires every
/// brief finding in exactly ONE of the two lists.
pub type FindingBounce {
  FindingBounce(finding_id: String, reason: String)
}

/// One declared deviation in a fix report (`fix-report.schema.json`).
pub type Deviation {
  Deviation(what: String, why: String, approved_by: String)
}

/// One same-pattern occurrence the developer found beyond the cited lines
/// (`fix-report.schema.json` `class_instances_found`): fixed in this brief,
/// or recorded for the verifier/re-audit.
pub type ClassInstance {
  ClassInstance(file: String, line: Int, fixed: Bool, note: String)
}

/// The developer's structured result per `fix-report.schema.json`
/// (2026-07-07 contract).
pub type FixReport {
  FixReport(
    brief_id: String,
    commits: List(String),
    findings_addressed: List(FindingFix),
    findings_bounced: List(FindingBounce),
    deviations: List(Deviation),
    new_tests: List(String),
    class_instances_found: List(ClassInstance),
  )
}

/// A verifier ruling for one finding (`verdict.schema.json` `ruling`).
pub type Ruling {
  Fixed
  Partial
  NotFixed
  RegressionIntroduced
}

/// One per-finding ruling with its concrete evidence (`verdict.schema.json`).
pub type FindingRuling {
  FindingRuling(finding_id: String, ruling: Ruling, evidence: String)
}

/// A surviving class sibling the verifier found (`verdict.schema.json`) —
/// each becomes a NEW ledger entry downstream.
pub type ClassSibling {
  ClassSibling(file: String, line: Int, description: String)
}

/// A cross-finding regression concern not tied to a single ruling
/// (`verdict.schema.json` `regression_risks`).
pub type RegressionRisk {
  RegressionRisk(file: String, concern: String)
}

/// The verdict's overall disposition (`verdict.schema.json` `overall`).
/// DERIVE-AND-CHECK: the workflow derives this mechanically from
/// `per_finding` (`remediation/checks.derive_overall`) and REJECTS a verdict
/// whose asserted value disagrees — consistency is checked, never trusted.
pub type Overall {
  Accept
  Reject
  PartialAccept
}

/// The verifier's structured result per `verdict.schema.json` (2026-07-07
/// contract). `standards_violations` empty is a deliberate statement (the
/// diff-wide scan ran and found nothing), not an omission.
pub type Verdict {
  Verdict(
    brief_id: String,
    per_finding: List(FindingRuling),
    class_siblings_found: List(ClassSibling),
    regression_risks: List(RegressionRisk),
    standards_violations: List(String),
    overall: Overall,
    reject_reason: Option(String),
  )
}

// --- shell activity payloads --------------------------------------------------

/// Input to the `provision_workspace` shell activity: create the brief's
/// isolated git worktree at `workspace_path`, checking out `branch` freshly
/// based on `base_branch`. The child derives `workspace_path` as
/// `<workspace_base>/<child_workflow_id>` so it matches the `--workspace-root`
/// the driven test-author/developer/verifier harnesses point Norn at.
pub type ProvisionInput {
  ProvisionInput(
    repo_root: String,
    base_branch: String,
    branch: String,
    workspace_path: String,
  )
}

/// Result of `provision_workspace`. `base_commit` pins the exact commit the
/// worktree started from — gate 1 computes the authored-test paths as the
/// files changed since it.
pub type WorkspaceInfo {
  WorkspaceInfo(workspace_path: String, branch: String, base_commit: String)
}

/// One runnable gate-1 check, routed from a manifest entry by the workflow
/// (pure): the finding's tests plus the substring their failing output MUST
/// contain (the fully mechanical fails-for-the-right-reason check).
pub type Gate1Check {
  Gate1Check(
    finding_id: String,
    test_names: List(String),
    expected_failure_signature: String,
  )
}

/// One manual-acceptance entry (improvement/completion findings with no
/// expressible failing test): nothing to run at gate 1; the criterion is
/// recorded for the verifier.
pub type AcceptanceCheck {
  AcceptanceCheck(finding_id: String, criterion: String)
}

/// Input to the `gate1` shell activity (2026-07-07 contract — fully
/// mechanical): per runnable manifest entry, re-run its tests, assert each
/// FAILS, assert the output contains the entry's signature; assert the
/// test-author's diff since `base_commit` touches ONLY test paths (the
/// manifest's `test_file` set plus the shared test-path rule); echo the
/// manual-acceptance entries into the result.
pub type Gate1Input {
  Gate1Input(
    workspace_path: String,
    base_commit: String,
    checks: List(Gate1Check),
    acceptance: List(AcceptanceCheck),
    test_files: List(String),
  )
}

/// One authored test's re-run: which finding it guards, whether it failed
/// (REQUIRED at gate 1), whether the captured output contained the entry's
/// `expected_failure_signature` (failing for the RIGHT reason), and the
/// captured output as evidence.
pub type TestRun {
  TestRun(
    finding_id: String,
    test_name: String,
    failed: Bool,
    signature_matched: Bool,
    evidence: String,
  )
}

/// Result of `gate1`. `pass` is true only when the authored tests are
/// committed, the authored diff stayed on test paths (`scope_violations`
/// empty), and every named test failed WITH its expected signature in the
/// output. `acceptance_checks` echoes the manual-acceptance entries (nothing
/// was run for them — recorded for the verifier). `authored_test_paths` and
/// `tests_commit` pin the immutable authored set for gate 2's tamper check.
/// A red check is recorded DATA, never an activity error.
pub type Gate1Outcome {
  Gate1Outcome(
    pass: Bool,
    results: List(TestRun),
    acceptance_checks: List(AcceptanceCheck),
    scope_violations: List(String),
    authored_test_paths: List(String),
    tests_commit: String,
    detail: String,
  )
}

/// Input to the `gate2` shell activity: the mechanical fix gate — the
/// authored-test-path diff since `tests_commit` must be empty, clippy
/// `-D warnings` must be green, and the full suite must be green.
pub type Gate2Input {
  Gate2Input(
    workspace_path: String,
    tests_commit: String,
    authored_test_paths: List(String),
  )
}

/// Result of `gate2`, each check recorded independently so a loop-back tells
/// the developer exactly what failed. `test_diff_clean == False` is a
/// test-edit attempt — a guard-failure metric, counted in the brief result.
/// `diff` carries the developer's full change (worktree vs `tests_commit`)
/// for the verifier.
pub type Gate2Outcome {
  Gate2Outcome(
    pass: Bool,
    test_diff_clean: Bool,
    clippy_pass: Bool,
    suite_pass: Bool,
    diagnostics: String,
    diff: String,
  )
}

/// Which stage artifact a ledger update applies (the applier CLI's `--kind`).
pub type ArtifactKind {
  TestManifestArtifact
  FixReportArtifact
  VerdictArtifact
  /// The applier's operator-signed disposition ruling (refuted|deferred;
  /// `signed_by` must be an operator — DECISIONS.md D9). Part of the `--kind`
  /// vocabulary this type mirrors, but the WORKFLOW never constructs it: its
  /// own terminal [`Disposition`] is a different concept, recorded on the
  /// [`BriefResult`], never sent to the applier
  /// (`remediation_brief.terminal_artifacts` pins this).
  DispositionArtifact
}

/// Input to the `ledger_update` shell activity: apply one stage artifact to
/// the in-repo ledger via `python3 scripts/remediation/apply_transitions.py
/// --ledger <ledger_path> --artifact <artifact.json> --kind <kind>` run in
/// `repo_root` (the applier is built in the yggdrasil repo; the CLI contract
/// is recorded in this example's README).
pub type LedgerUpdateInput {
  LedgerUpdateInput(
    repo_root: String,
    ledger_path: String,
    kind: ArtifactKind,
    artifact_json: String,
  )
}

/// Result of `ledger_update`: whether the applier accepted the artifact. A
/// non-zero applier exit is recorded honestly as `applied: False` (and carried
/// into the brief result), never swallowed into a success.
pub type LedgerUpdateOutcome {
  LedgerUpdateOutcome(applied: Bool, detail: String)
}

/// Input to the `cleanup_workspace` shell activity: remove the brief's
/// worktree (the branch, and the work on it, remain).
pub type CleanupInput {
  CleanupInput(repo_root: String, workspace_path: String)
}

/// Result of `cleanup_workspace`. A dirty worktree is NOT removed (that would
/// destroy uncommitted work — the very defect class Wave 0 exists to fix);
/// `removed: False` with the reason is the honest record.
pub type CleanupOutcome {
  CleanupOutcome(removed: Bool, detail: String)
}

// --- child result --------------------------------------------------------------

/// The terminal disposition of one brief's run. Exhaustion and gate-1 failure
/// are terminal DISPOSITIONS recorded in durable history — never a silent
/// success, and not workflow errors: the parent still collects them into the
/// wave report.
pub type Disposition {
  /// Every ruling in the final verdict is `fixed`.
  Accepted
  /// The authored tests did not clear the fail-first gate (uncommitted tests,
  /// a test that passed on unfixed code, or a correction finding with neither
  /// a test nor a `could_not_reproduce` flag).
  Gate1Failed
  /// The developer-round budget ran out before an accepting verdict.
  CycleCapExhausted
}

/// One ledger-applier invocation's recorded outcome, kept on the brief result
/// so an unapplied transition is visible to the operator, never silent.
pub type LedgerApplication {
  LedgerApplication(kind: String, applied: Bool, detail: String)
}

/// The child `remediation_brief` workflow's result: the terminal disposition
/// plus everything the operator and the wave report need — cycle accounting,
/// the `could_not_reproduce` finding ids (D4: surfaced, not rerouted), the
/// artifacts, the ledger-application record, and every verdict-consistency
/// violation caught by the derive-and-check rule (`verdict_mismatches` —
/// asserted-vs-derived disagreements and missing reject reasons, cycle-
/// stamped; evidence for the operator, never silently accepted).
pub type BriefResult {
  BriefResult(
    brief_id: String,
    disposition: Disposition,
    fix_cycles: Int,
    first_pass_accepted: Bool,
    could_not_reproduce: List(String),
    test_edit_attempts: Int,
    verdict_mismatches: List(String),
    branch: String,
    manifest: TestManifest,
    fix_report: Option(FixReport),
    verdict: Option(Verdict),
    ledger: List(LedgerApplication),
    workspace_removed: Bool,
    summary: String,
  )
}

// --- parent (remediation_wave) I/O ---------------------------------------------

/// One brief in a wave plan, paired with its member ledger entries (the child
/// needs them; the planner emits them alongside the brief).
pub type WaveBrief {
  WaveBrief(brief: Brief, entries: List(LedgerEntry))
}

/// Input to the parent `remediation_wave` workflow: the signed wave plan
/// (DECISIONS.md D6 — triage and Gate 0 happen OUTSIDE the workflow; this is
/// the approved plan). Strata run serially; briefs within a stratum run in
/// parallel as child workflows.
pub type WaveInput {
  WaveInput(
    briefs: List(WaveBrief),
    strata: List(List(String)),
    config: RunConfig,
  )
}

/// Per-stage wave metrics (`wave-report.schema.json` `metrics`), each field
/// `None` when this run cannot compute it (emitted as JSON `null` — the
/// ledger-keeper completes the report; see README).
pub type TestAuthoringMetrics {
  TestAuthoringMetrics(
    valid_fail_first_rate: Option(Float),
    wrong_reason_fail_rate: Option(Float),
    could_not_reproduce_rate: Option(Float),
  )
}

/// Fix-stage metrics (`wave-report.schema.json` `metrics.fix`).
pub type FixMetrics {
  FixMetrics(
    first_pass_acceptance_rate: Option(Float),
    fix_cycles_per_brief: Option(Float),
    deviation_count: Option(Int),
    test_edit_attempts: Option(Int),
  )
}

/// Verify-stage metrics (`wave-report.schema.json` `metrics.verify`).
pub type VerifyMetrics {
  VerifyMetrics(
    class_siblings_per_brief: Option(Float),
    verdicts_overturned: Option(Int),
  )
}

/// Re-audit-stage metrics (`wave-report.schema.json` `metrics.re_audit`) —
/// stage 4 does not run in this workflow yet, so both are always `None` here.
pub type ReAuditMetrics {
  ReAuditMetrics(
    class_recurrence_rate: Option(Float),
    new_finding_inflow: Option(Int),
  )
}

/// Flow metrics (`wave-report.schema.json` `metrics.flow`) — computed from
/// ledger history by the ledger-keeper, so always `None` here.
pub type FlowMetrics {
  FlowMetrics(
    lead_time_days: Option(Float),
    terminal_state_ratio: Option(Float),
  )
}

/// The metrics block of the wave report skeleton.
pub type WaveMetrics {
  WaveMetrics(
    test_authoring: TestAuthoringMetrics,
    fix: FixMetrics,
    verify: VerifyMetrics,
    re_audit: ReAuditMetrics,
    flow: FlowMetrics,
  )
}

/// The wave report SKELETON per `wave-report.schema.json`: metrics filled
/// where this run can compute them, everything ledger-derived left empty/null
/// for the ledger-keeper (ledger delta, queues, finder calibration).
pub type WaveReport {
  WaveReport(
    wave: Int,
    new_entries: List(String),
    metrics: WaveMetrics,
    deferred_queue: List(String),
    refuted_queue: List(String),
  )
}

/// The parent `remediation_wave` workflow's result: every brief's terminal
/// result plus the wave report skeleton.
pub type WaveResult {
  WaveResult(
    wave: Int,
    briefs: List(BriefResult),
    report: WaveReport,
    summary: String,
  )
}

// --- typed errors -----------------------------------------------------------------

/// Parent/child workflow failures, surfaced as typed data in the run history.
/// Terminal DISPOSITIONS (cap exhaustion, gate-1 failure) are results, not
/// errors — these variants are the engine/activity/plan faults.
pub type RemediationError {
  /// A named stage failed as an activity error.
  StageFailed(stage: String, message: String)
  /// The wave plan's strata are not runnable (unknown/duplicate/missing brief
  /// ids) — every rejection names the offending id.
  StrataInvalid(reason: String)
  /// A child `remediation_brief` run failed at the engine/child boundary.
  ChildFailed(reason: String)
  /// The workflow input could not be decoded.
  DecodeInputFailed(message: String)
}

// --- string renderings ----------------------------------------------------------

/// The wire tag for a category.
pub fn category_to_string(category: Category) -> String {
  case category {
    Correction -> "correction"
    Completion -> "completion"
    Improvement -> "improvement"
  }
}

/// Resolve a category tag; unknown tags are a decode failure upstream.
pub fn category_from_string(tag: String) -> Option(Category) {
  case tag {
    "correction" -> option.Some(Correction)
    "completion" -> option.Some(Completion)
    "improvement" -> option.Some(Improvement)
    _ -> option.None
  }
}

/// The wire tag for a verifier ruling.
pub fn ruling_to_string(ruling: Ruling) -> String {
  case ruling {
    Fixed -> "fixed"
    Partial -> "partial"
    NotFixed -> "not_fixed"
    RegressionIntroduced -> "regression_introduced"
  }
}

/// Resolve a ruling tag; unknown tags are a decode failure upstream.
pub fn ruling_from_string(tag: String) -> Option(Ruling) {
  case tag {
    "fixed" -> option.Some(Fixed)
    "partial" -> option.Some(Partial)
    "not_fixed" -> option.Some(NotFixed)
    "regression_introduced" -> option.Some(RegressionIntroduced)
    _ -> option.None
  }
}

/// The wire tag for a verdict overall.
pub fn overall_to_string(overall: Overall) -> String {
  case overall {
    Accept -> "accept"
    Reject -> "reject"
    PartialAccept -> "partial_accept"
  }
}

/// Resolve an overall tag; unknown tags are a decode failure upstream.
pub fn overall_from_string(tag: String) -> Option(Overall) {
  case tag {
    "accept" -> option.Some(Accept)
    "reject" -> option.Some(Reject)
    "partial_accept" -> option.Some(PartialAccept)
    _ -> option.None
  }
}

/// The wire tag for a terminal disposition.
pub fn disposition_to_string(disposition: Disposition) -> String {
  case disposition {
    Accepted -> "accepted"
    Gate1Failed -> "gate1_failed"
    CycleCapExhausted -> "cycle_cap_exhausted"
  }
}

/// Resolve a disposition tag; unknown tags are a decode failure upstream.
pub fn disposition_from_string(tag: String) -> Option(Disposition) {
  case tag {
    "accepted" -> option.Some(Accepted)
    "gate1_failed" -> option.Some(Gate1Failed)
    "cycle_cap_exhausted" -> option.Some(CycleCapExhausted)
    _ -> option.None
  }
}

/// The wire tag for an artifact kind — exactly the applier CLI's `--kind`
/// vocabulary.
pub fn artifact_kind_to_string(kind: ArtifactKind) -> String {
  case kind {
    TestManifestArtifact -> "test_manifest"
    FixReportArtifact -> "fix_report"
    VerdictArtifact -> "verdict"
    DispositionArtifact -> "disposition"
  }
}

/// Resolve an artifact-kind tag; unknown tags are a decode failure upstream.
pub fn artifact_kind_from_string(tag: String) -> Option(ArtifactKind) {
  case tag {
    "test_manifest" -> option.Some(TestManifestArtifact)
    "fix_report" -> option.Some(FixReportArtifact)
    "verdict" -> option.Some(VerdictArtifact)
    "disposition" -> option.Some(DispositionArtifact)
    _ -> option.None
  }
}
