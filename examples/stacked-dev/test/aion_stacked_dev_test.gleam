//// Behavioral tests for the stacked-dev workflow family.
////
//// Every test runs the REAL workflow bodies under the `aion/testing`
//// harness: both child workflows execute their genuine `execute` functions
//// through `workflow.spawn_and_wait`, every activity executes its genuine
//// CLI-shelling local implementation, and fake-CLI shims (per-test scripts
//// placed alone on `PATH`) intercept at the process boundary while
//// recording their argv. Signals are queued through `signal.send`, exactly
//// the channel `aion signal <run-id> review_verdict --payload '{...}'`
//// drives on a live server.

import aion/activity
import aion/error
import aion/query
import aion/signal
import aion/testing
import aion_stacked_dev_io as stage_io
import brief_dev
import gleam/list
import gleam/option
import gleam/string
import gleeunit
import gleeunit/should
import stacked_dev
import stacked_dev/activities
import stacked_dev/codecs_brief
import stacked_dev/codecs_flow
import stacked_dev/codecs_workflows
import stacked_dev/enrich
import stacked_dev/locals
import stacked_dev/types.{
  type BriefDocument, type ResolvedContext, type ReviewVerdict,
  type StackedDevInput, Approve, AttestationBlock, BriefDevStatus, BriefDocument,
  BriefRequirement, DevEnrichment, EnrichInput, ExecutionBlock,
  ExecutionEnrichment, ExecutionLanded, GateBlock, GateRejected, Local,
  ProvisionFailed, Reject, RequestChanges, RequirementFiles, ResolvedContext,
  ResolvedProvenance, ReviewCapExhausted, ReviewEnrichment, ReviewNote,
  ReviewRejected, ReviewTimedOut, ReviewVerdict, ScoutBlock, ScoutEnrichment,
  StackedDevInput, StackedDevStatus, VerdictApproved, VerifyExhausted, Workspace,
  Worktree,
}
import support/fixtures
import support/shims

pub fn main() {
  gleeunit.main()
}

/// Workflow input used by every scenario. Caps, backoff, and deadline are
/// required fields (open question Q5), so each test states them explicitly.
/// `repo_root` is the shim directory: provision creates the worktree under it,
/// so every downstream activity holds a real, absolute working directory.
fn base_input(shim_set: shims.Shims) -> StackedDevInput {
  StackedDevInput(
    repo_root: shim_set.root,
    brief_id: "brief-7",
    reviewers: ["sample-reviewer"],
    base_ref: "main",
    placement: Local,
    isolation: Worktree,
    brief_document: base_document(),
    resolved_context: base_context(),
    verify_fix_cap: 3,
    review_cap: 3,
    round_backoff_ms: 25,
    review_deadline_ms: 60_000,
    clone_url: option.None,
  )
}

/// A one-requirement authored brief document the pipeline scenarios carry.
/// brief_dev does not enrich the document, so the requirement id need not
/// match the shim reports' ids.
fn base_document() -> BriefDocument {
  BriefDocument(
    id: "BD-007",
    cluster: "brief-dev",
    title: "Implement the stacked-dev example",
    depends_on: [],
    blocked_by: [],
    checklist: [],
    stories: [],
    design_anchor: [],
    purpose: "Exercise the pipeline end to end.",
    task: "Implement R1.",
    requirements: [
      BriefRequirement(
        id: "R1",
        title: "The variant",
        spec: "Add the variant.",
        acceptance: ["the variant exists"],
        files: RequirementFiles(create: [], modify: [], delete: []),
        checklist: [],
        stories: [],
        scout: option.None,
        dev: option.None,
        review: option.None,
      ),
    ],
    boundaries: ["No scope creep."],
    verification: ["gleam test"],
    execution: option.None,
  )
}

/// A minimal pre-resolved context the pipeline scenarios carry.
fn base_context() -> ResolvedContext {
  ResolvedContext(
    adrs: [],
    checklist: [],
    stories: [],
    constraints: [],
    intention: "Design-system v2 briefs become executable.",
    design_path: "docs/design/brief-dev/design.json",
    provenance: ResolvedProvenance(requested_by: "Tom", quote: ""),
  )
}

/// Fresh harness env + shim dir with the full pipeline (real local impls and
/// real children) registered, the standard `meridian`/`norn` shims installed,
/// and the scenario-specific check shims (`cargo` warm build + `yg` checks)
/// installed by the caller.
fn pipeline(
  install_checks: fn(shims.Shims) -> Nil,
) -> #(testing.TestEnv, shims.Shims) {
  let #(env, shim_set) = bare_pipeline()
  shims.write_meridian(shim_set)
  shims.write_norn(shim_set)
  shims.write_git(shim_set)
  install_checks(shim_set)
  #(env, shim_set)
}

/// Fresh harness env + an EMPTY shim dir on `PATH`: every CLI is genuinely
/// absent.
fn bare_pipeline() -> #(testing.TestEnv, shims.Shims) {
  let assert Ok(env) = testing.new()
  let shim_set = shims.install()
  shims.register_pipeline(env)
  #(env, shim_set)
}

/// All checks pass: warm build succeeds, scoped and workspace diagnostics are
/// clean.
fn checks_passing(shim_set: shims.Shims) -> Nil {
  shims.write_cargo(shim_set)
  shims.write_yg_passing(shim_set)
}

/// Scoped diagnostics fail `failures` times then pass; the warm build and the
/// workspace gate are clean.
fn checks_scoped_fail(failures: Int) -> fn(shims.Shims) -> Nil {
  fn(shim_set: shims.Shims) {
    shims.write_cargo(shim_set)
    shims.write_yg_failing_scoped(shim_set, failures)
  }
}

/// Scoped diagnostics pass; only the workspace gate fails.
fn checks_workspace_fail(shim_set: shims.Shims) -> Nil {
  shims.write_cargo(shim_set)
  shims.write_yg_failing_workspace(shim_set)
}

/// The warm build fails (advisory); all diagnostics pass.
fn checks_warm_fail(shim_set: shims.Shims) -> Nil {
  shims.write_cargo_failing_build(shim_set)
  shims.write_yg_passing(shim_set)
}

fn send_verdict(verdict: ReviewVerdict) -> Nil {
  let assert Ok(Nil) =
    signal.send("stacked-dev-test-run", stacked_dev.review_signal(), verdict)
  Nil
}

pub fn full_pipeline_happy_path_approves_first_round_test() {
  let #(_env, shim_set) = pipeline(checks_passing)
  send_verdict(ReviewVerdict(decision: Approve))

  let assert Ok(result) = stacked_dev.execute(base_input(shim_set))

  result.branch |> should.equal(shims.landed_branch)
  result.merged_into |> should.equal(shims.merged_into)
  result.session_id |> should.equal(shims.session_id)
  result.build_warm.ok |> should.be_true
  result.verify_rounds |> should.equal(1)
  result.review_rounds |> should.equal(1)

  // Provisioning is two real yg verbs: add the branch, then provision it.
  shims.invocations(shim_set, "yg", "branch add") |> should.equal(1)
  shims.invocations(shim_set, "yg", "branch provision") |> should.equal(1)

  // Land commits the dev rounds' files on the branch, then merges it into
  // its parent via yg, exactly once, after review approved.
  shims.invocations(shim_set, "git", "add -A") |> should.equal(1)
  shims.invocations(shim_set, "git", "commit -m " <> shims.landed_branch)
  |> should.equal(1)
  shims.invocations(shim_set, "yg", "branch merge " <> shims.landed_branch)
  |> should.equal(1)
  // The review request led with the branch positional (the greedy
  // `--reviewer` flag would otherwise swallow it) and signed as Meridian.
  shims.log(shim_set, "meridian")
  |> string.contains(
    "review request "
    <> shims.landed_branch
    <> " --reviewer sample-reviewer --as Meridian",
  )
  |> should.be_true

  // The startup fan-out really warmed the cache concurrently with dev.
  shims.invocations(shim_set, "cargo", "build") |> should.equal(1)
  // Three deterministic-session norn rounds ran: scout (<branch>-scout), dev
  // (<branch>), and the adversarial review (<branch>-review).
  shims.invocations(shim_set, "norn", "--print --session-id")
  |> should.equal(3)
}

pub fn verify_fix_loop_converges_on_round_two_test() {
  let #(_env, shim_set) = pipeline(checks_scoped_fail(1))
  send_verdict(ReviewVerdict(decision: Approve))

  let assert Ok(result) = stacked_dev.execute(base_input(shim_set))

  // Round 1 failed scoped diagnostics, dev_resume fed the diagnostics back,
  // and round 2 converged.
  result.verify_rounds |> should.equal(2)
  result.review_rounds |> should.equal(1)
  shims.invocations(shim_set, "yg", "diagnostics check --format json --package")
  |> should.equal(2)

  // The scoped-check diagnostics reached the resumed agent's argv intact.
  let norn_log = shims.log(shim_set, "norn")
  norn_log
  |> string.contains("--resume " <> shims.session_id)
  |> should.be_true
  norn_log |> string.contains(shims.scoped_diagnostics) |> should.be_true
}

pub fn verify_fix_exhaustion_surfaces_typed_diagnostics_test() {
  // Scoped diagnostics never pass.
  let #(_env, shim_set) = pipeline(checks_scoped_fail(1_000_000))

  let input = StackedDevInput(..base_input(shim_set), verify_fix_cap: 2)
  let assert Error(VerifyExhausted(rounds: rounds, diagnostics: diagnostics)) =
    stacked_dev.execute(input)

  rounds |> should.equal(2)
  diagnostics |> string.contains(shims.scoped_diagnostics) |> should.be_true
  diagnostics |> string.contains("aion-core") |> should.be_true

  // The run never reached the gate, review, or land stages.
  shims.invocations(shim_set, "yg", "diagnostics check --workspace")
  |> should.equal(0)
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(0)
  shims.invocations(shim_set, "yg", "branch merge") |> should.equal(0)

  // The child's status query reports where it stopped: still verifying at
  // the capped round.
  query.dispatch(
    brief_dev.status_query_name,
    codecs_workflows.brief_dev_status_codec(),
  )
  |> should.equal(Ok(BriefDevStatus(phase: "verifying", round: 2)))
}

pub fn review_request_changes_notes_reach_dev_resume_and_regate_test() {
  let #(_env, shim_set) = pipeline(checks_passing)
  send_verdict(
    ReviewVerdict(
      decision: RequestChanges(notes: [
        ReviewNote(
          file: "crates/aion-core/src/lib.rs",
          line: 42,
          note: "tighten the error taxonomy",
        ),
      ]),
    ),
  )
  send_verdict(ReviewVerdict(decision: Approve))

  let assert Ok(result) = stacked_dev.execute(base_input(shim_set))

  result.review_rounds |> should.equal(2)
  result.verify_rounds |> should.equal(1)

  // The structured notes (open question Q3) reached the resumed agent's
  // argv as data: file, line, and note all present in the recorded feedback.
  let norn_log = shims.log(shim_set, "norn")
  norn_log
  |> string.contains("--resume " <> shims.session_id)
  |> should.be_true
  norn_log |> string.contains("crates/aion-core/src/lib.rs") |> should.be_true
  norn_log |> string.contains("\"line\":42") |> should.be_true
  norn_log |> string.contains("tighten the error taxonomy") |> should.be_true

  // Each fix round re-gates: the workspace gate ran twice, the review was
  // requested twice, and the branch merged once.
  shims.invocations(shim_set, "yg", "diagnostics check --workspace")
  |> should.equal(2)
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(2)
  shims.invocations(shim_set, "yg", "branch merge")
  |> should.equal(1)
}

pub fn gate_failure_after_convergence_is_typed_gate_rejected_test() {
  // Scoped checks pass (the fast loop converges), but the authoritative
  // workspace gate catches a cross-crate failure: the run fails loudly with
  // the gate's report instead of looping or reaching review.
  let #(_env, shim_set) = pipeline(checks_workspace_fail)

  let assert Error(GateRejected(report: report)) =
    stacked_dev.execute(base_input(shim_set))

  report |> string.contains(shims.workspace_report) |> should.be_true
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(0)
  shims.invocations(shim_set, "yg", "branch merge") |> should.equal(0)
}

pub fn review_cap_exhaustion_fails_the_run_with_typed_rounds_test() {
  // One review round allowed; the reviewer requests changes, the fix
  // re-gates cleanly, and the next round would exceed the cap — a typed
  // ReviewCapExhausted, never an infinite review loop.
  let #(_env, shim_set) = pipeline(checks_passing)
  send_verdict(
    ReviewVerdict(
      decision: RequestChanges(notes: [
        ReviewNote(
          file: "crates/aion-core/src/lib.rs",
          line: 7,
          note: "round one is never enough",
        ),
      ]),
    ),
  )

  let input = StackedDevInput(..base_input(shim_set), review_cap: 1)
  stacked_dev.execute(input)
  |> should.equal(Error(ReviewCapExhausted(rounds: 1)))

  // Exactly one review round ran; the fix was re-gated; nothing landed.
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(1)
  shims.invocations(shim_set, "yg", "diagnostics check --workspace")
  |> should.equal(2)
  shims.invocations(shim_set, "yg", "branch merge") |> should.equal(0)
}

pub fn review_reject_fails_the_run_with_typed_reason_test() {
  let #(_env, shim_set) = pipeline(checks_passing)
  send_verdict(ReviewVerdict(decision: Reject(reason: "wrong architecture")))

  stacked_dev.execute(base_input(shim_set))
  |> should.equal(Error(ReviewRejected(reason: "wrong architecture")))

  // A rejected run never lands.
  shims.invocations(shim_set, "yg", "branch merge") |> should.equal(0)
}

pub fn review_timeout_fails_the_run_with_typed_deadline_test() {
  let #(_env, shim_set) = pipeline(checks_passing)
  // No verdict is ever sent; the durable deadline expires instead.
  let input = StackedDevInput(..base_input(shim_set), review_deadline_ms: 0)

  stacked_dev.execute(input)
  |> should.equal(Error(ReviewTimedOut(deadline_ms: 0)))

  // The review was requested, but nothing was landed.
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(1)
  shims.invocations(shim_set, "yg", "branch merge") |> should.equal(0)
}

pub fn warm_build_failure_is_advisory_and_never_fails_the_run_test() {
  let #(_env, shim_set) = pipeline(checks_warm_fail)
  send_verdict(ReviewVerdict(decision: Approve))

  let assert Ok(result) = stacked_dev.execute(base_input(shim_set))

  // The forfeited cache is recorded as advisory data; the run still landed.
  result.build_warm.ok |> should.be_false
  result.branch |> should.equal(shims.landed_branch)
  result.review_rounds |> should.equal(1)
  shims.invocations(shim_set, "cargo", "build") |> should.equal(1)
}

pub fn status_query_answers_live_phase_and_round_per_stage_test() {
  // A landed run reports the terminal phase with its review round, and the
  // child reports its converged verify round.
  let #(_env, shim_set) = pipeline(checks_passing)
  send_verdict(ReviewVerdict(decision: Approve))
  let assert Ok(_) = stacked_dev.execute(base_input(shim_set))
  query.dispatch(
    stacked_dev.status_query_name,
    codecs_workflows.stacked_dev_status_codec(),
  )
  |> should.equal(Ok(StackedDevStatus(phase: "landed", round: 1)))
  query.dispatch(
    brief_dev.status_query_name,
    codecs_workflows.brief_dev_status_codec(),
  )
  |> should.equal(Ok(BriefDevStatus(phase: "converged", round: 1)))

  // A rejected run stops with the handler registered for the review phase.
  let #(_env, shim_set) = pipeline(checks_passing)
  send_verdict(ReviewVerdict(decision: Reject(reason: "no")))
  let assert Error(ReviewRejected(reason: "no")) =
    stacked_dev.execute(base_input(shim_set))
  query.dispatch(
    stacked_dev.status_query_name,
    codecs_workflows.stacked_dev_status_codec(),
  )
  |> should.equal(Ok(StackedDevStatus(phase: "in_review", round: 1)))
}

pub fn missing_cli_with_no_shim_is_a_loud_activity_failure_test() {
  // PATH points at an empty shim directory: no yg, no norn, no cargo. The very
  // first activity (provision -> yg branch add) must fail loudly, naming the
  // absent executable — activities are never silently skipped.
  let #(_env, shim_set) = bare_pipeline()

  let assert Error(ProvisionFailed(message: message)) =
    stacked_dev.execute(base_input(shim_set))
  message
  |> string.contains("executable not found on PATH: yg")
  |> should.be_true
}

// --- enrichment (BD-004) -----------------------------------------------------
//
// Pure append-only merge semantics (enrich.gleam), the enrich_input codec,
// and the enrich_brief activity local implementation. These tests are pure
// of the CLI seam — no shims are installed — and the activity tests exercise
// the real file boundary against per-test directories under /tmp.

const enrich_fixture_path = "../../docs/design/brief-dev/briefs/BD-001.json"

const enriched_fixture_dir = "/tmp/aion-stacked-dev-enrich"

const enriched_fixture_file = "/tmp/aion-stacked-dev-enrich/briefs/enriched-fixture.json"

@external(erlang, "stacked_dev_file_ffi", "write_file")
fn write_file(path: String, contents: String) -> Result(Nil, String)

@external(erlang, "stacked_dev_file_ffi", "remove_tree")
fn remove_tree(path: String) -> Result(Nil, String)

/// A two-requirement (R1, R2) authored brief document, enrichment-free.
fn enrich_document() -> types.BriefDocument {
  BriefDocument(
    id: "BD-900",
    cluster: "brief-dev",
    title: "Enrichment test brief",
    depends_on: [],
    blocked_by: [],
    checklist: ["C19"],
    stories: ["S12"],
    design_anchor: ["ADR-007"],
    purpose: "Exercise the append-only merge.",
    task: "Merge and inspect.",
    requirements: [enrich_requirement("R1"), enrich_requirement("R2")],
    boundaries: ["No authored field changes."],
    verification: ["gleam test"],
    execution: option.None,
  )
}

fn enrich_requirement(id: String) -> types.BriefRequirement {
  BriefRequirement(
    id: id,
    title: "Requirement " <> id,
    spec: "Spec for " <> id <> ".",
    acceptance: ["Acceptance for " <> id <> "."],
    files: RequirementFiles(create: [], modify: [], delete: []),
    checklist: ["C19"],
    stories: ["S12"],
    scout: option.None,
    dev: option.None,
    review: option.None,
  )
}

fn scout_entry(
  id: String,
  notes: String,
) -> stage_io.ScoutReportEnrichmentsItem {
  stage_io.ScoutReportEnrichmentsItem(
    id: id,
    files: ["file-" <> id],
    context: ["context-" <> id],
    approach: "approach-" <> id,
    notes: notes,
  )
}

fn scout_report(
  entries: List(stage_io.ScoutReportEnrichmentsItem),
) -> stage_io.ScoutReport {
  stage_io.ScoutReport(
    summary: "scout summary",
    enrichments: entries,
    verification: ["gleam test"],
  )
}

fn dev_entry(id: String) -> stage_io.DevReportEnrichmentsItem {
  stage_io.DevReportEnrichmentsItem(
    id: id,
    status: stage_io.DevReportEnrichmentsItemStatusImplemented,
    files_changed: [
      stage_io.DevReportEnrichmentsItemFilesChangedItem(
        path: "src/file-" <> id <> ".gleam",
        change: stage_io.DevReportEnrichmentsItemFilesChangedItemChangeModified,
        note: "dev note for " <> id,
      ),
    ],
    how: "how-" <> id,
    deviation: "",
    checklist: [
      stage_io.DevReportEnrichmentsItemChecklistItem(
        id: "C19",
        done: True,
        note: "delivered",
      ),
    ],
    stories: [
      stage_io.DevReportEnrichmentsItemStoriesItem(
        id: "S12",
        satisfied: True,
        note: "satisfied",
      ),
    ],
  )
}

fn dev_report(
  entries: List(stage_io.DevReportEnrichmentsItem),
) -> stage_io.DevReport {
  stage_io.DevReport(
    summary: "dev summary",
    commit_message: "BD-900: enrichment test",
    enrichments: entries,
    attestation: stage_io.DevReportAttestation(
      no_panics: True,
      no_unsafe: True,
      boundaries_respected: True,
      tests_pass: True,
    ),
  )
}

fn review_entry(id: String) -> stage_io.ReviewReportEnrichmentsItem {
  stage_io.ReviewReportEnrichmentsItem(
    id: id,
    alignment: stage_io.ReviewReportEnrichmentsItemAlignmentAligned,
    acceptance: [
      stage_io.ReviewReportEnrichmentsItemAcceptanceItem(
        criterion: "Acceptance for " <> id <> ".",
        met: True,
        evidence: "evidence for " <> id,
      ),
    ],
    checklist: ["C19"],
    stories: ["S12"],
    issues: [],
    fixes: [],
  )
}

fn review_report(
  entries: List(stage_io.ReviewReportEnrichmentsItem),
) -> stage_io.ReviewReport {
  stage_io.ReviewReport(
    summary: "review summary",
    commit_message: "BD-900: review fixes",
    enrichments: entries,
    verification: [
      stage_io.ReviewReportVerificationItem(
        criterion: "gleam test",
        passed: True,
        note: "",
      ),
    ],
  )
}

/// The measured gate and the believed attestation stay separate fields (P1);
/// `landed_commit` stays empty because a commit cannot name itself (ADR-009).
fn sample_execution_block(workflow_id: String) -> types.ExecutionBlock {
  ExecutionBlock(
    status: ExecutionLanded,
    workflow_id: workflow_id,
    branch: "stacked-dev-BD-900",
    session_id: "stacked-dev-BD-900",
    gate: GateBlock(fmt: True, clippy: True, tests: True, fix_rounds: 0),
    attestation: AttestationBlock(
      no_panics: True,
      no_unsafe: True,
      boundaries_respected: True,
      tests_pass: True,
    ),
    review_verdict: VerdictApproved,
    landed_commit: "",
    merged_into: "main",
    completed_at: "2026-06-13T00:00:00Z",
  )
}

fn enrich_workspace(root: String) -> types.Workspace {
  Workspace(
    path: root,
    branch: "stacked-dev-BD-900",
    placement: Local,
    isolation: Worktree,
  )
}

/// A fresh per-test workspace root with the document seeded at the brief's
/// design-system path; returns that path.
fn seed_brief(root: String, document: types.BriefDocument) -> String {
  let path =
    root
    <> "/docs/design/"
    <> document.cluster
    <> "/briefs/"
    <> document.id
    <> ".json"
  let assert Ok(Nil) = remove_tree(root)
  let assert Ok(Nil) =
    write_file(path, codecs_brief.brief_document_codec().encode(document))
  path
}

pub fn merge_scout_appends_blocks_and_keeps_authored_subset_stable_test() {
  let document = enrich_document()
  let report =
    scout_report([
      scout_entry("R1", "watch the codec ordering"),
      scout_entry("R2", ""),
    ])

  let assert Ok(merged) = enrich.merge_scout(document, report)

  let assert [first, second] = merged.requirements
  first.scout
  |> should.equal(
    option.Some(ScoutBlock(
      files: ["file-R1"],
      context: ["context-R1"],
      approach: "approach-R1",
      notes: "watch the codec ordering",
    )),
  )
  second.scout
  |> should.equal(
    option.Some(ScoutBlock(
      files: ["file-R2"],
      context: ["context-R2"],
      approach: "approach-R2",
      notes: "",
    )),
  )

  // The encoded authored subset is byte-identical before and after the merge.
  let brief_codec = codecs_brief.brief_document_codec()
  brief_codec.encode(enrich.authored_subset(merged))
  |> should.equal(brief_codec.encode(enrich.authored_subset(document)))
}

pub fn merge_scout_replaces_an_existing_block_wholesale_test() {
  let document = enrich_document()
  let assert Ok(once) =
    enrich.merge_scout(
      document,
      scout_report([scout_entry("R1", "watch the codec ordering")]),
    )
  let assert Ok(twice) =
    enrich.merge_scout(once, scout_report([scout_entry("R1", "")]))

  // The block was replaced wholesale, never field-merged: the second
  // report's empty notes win.
  let assert [first, ..] = twice.requirements
  let assert option.Some(block) = first.scout
  block.notes |> should.equal("")
}

pub fn merge_dev_leaves_scout_blocks_untouched_test() {
  let document = enrich_document()
  let assert Ok(scouted) =
    enrich.merge_scout(
      document,
      scout_report([scout_entry("R1", "a"), scout_entry("R2", "b")]),
    )
  let assert Ok(developed) =
    enrich.merge_dev(scouted, dev_report([dev_entry("R1"), dev_entry("R2")]))

  // Cross-stage isolation: every scout block is byte-identical, only the dev
  // blocks were set, and review stays absent.
  list.map(developed.requirements, fn(requirement) { requirement.scout })
  |> should.equal(
    list.map(scouted.requirements, fn(requirement) { requirement.scout }),
  )
  list.all(developed.requirements, fn(requirement) {
    requirement.dev != option.None && requirement.review == option.None
  })
  |> should.be_true
}

pub fn merge_execution_sets_then_replaces_wholesale_test() {
  let document = enrich_document()
  document.execution |> should.equal(option.None)

  let assert Ok(once) =
    enrich.merge_execution(document, sample_execution_block("wf-1"))
  once.execution |> should.equal(option.Some(sample_execution_block("wf-1")))

  let assert Ok(twice) =
    enrich.merge_execution(once, sample_execution_block("wf-2"))
  twice.execution |> should.equal(option.Some(sample_execution_block("wf-2")))
}

pub fn merge_unknown_requirement_id_is_a_loud_error_test() {
  enrich.merge_scout(enrich_document(), scout_report([scout_entry("R9", "")]))
  |> should.equal(Error(enrich.UnknownRequirementId(id: "R9")))
}

pub fn enrich_input_codec_round_trips_every_variant_test() {
  let input_codec = codecs_flow.enrich_input_codec()
  let document = enrich_document()
  [
    ScoutEnrichment(report: scout_report([scout_entry("R1", "n")])),
    DevEnrichment(report: dev_report([dev_entry("R1")])),
    ReviewEnrichment(report: review_report([review_entry("R1")])),
    ExecutionEnrichment(block: sample_execution_block("wf-1")),
  ]
  |> list.each(fn(enrichment) {
    let input =
      EnrichInput(
        workspace: enrich_workspace("/w"),
        document: document,
        enrichment: enrichment,
      )
    input_codec.decode(input_codec.encode(input)) |> should.equal(Ok(input))
  })
}

pub fn enrich_input_codec_uses_documented_stage_tags_test() {
  let input_codec = codecs_flow.enrich_input_codec()
  let document = enrich_document()
  let scout_input =
    EnrichInput(
      workspace: enrich_workspace("/w"),
      document: document,
      enrichment: ScoutEnrichment(report: scout_report([scout_entry("R1", "")])),
    )
  let execution_input =
    EnrichInput(
      workspace: enrich_workspace("/w"),
      document: document,
      enrichment: ExecutionEnrichment(block: sample_execution_block("wf-1")),
    )
  input_codec.encode(scout_input)
  |> string.contains("\"stage\":\"scout\"")
  |> should.be_true
  input_codec.encode(execution_input)
  |> string.contains("\"stage\":\"execution\"")
  |> should.be_true
}

pub fn enrich_input_codec_rejects_undocumented_stage_tags_test() {
  let input_codec = codecs_flow.enrich_input_codec()
  let encoded =
    input_codec.encode(EnrichInput(
      workspace: enrich_workspace("/w"),
      document: enrich_document(),
      enrichment: ScoutEnrichment(report: scout_report([scout_entry("R1", "")])),
    ))
  string.replace(encoded, "\"stage\":\"scout\"", "\"stage\":\"harden\"")
  |> input_codec.decode
  |> should.be_error
}

pub fn enrich_brief_activity_name_matches_the_constant_test() {
  activities.enrich_brief_name |> should.equal("enrich_brief")
  let built =
    activities.enrich_brief(EnrichInput(
      workspace: enrich_workspace("/w"),
      document: enrich_document(),
      enrichment: ExecutionEnrichment(block: sample_execution_block("wf-1")),
    ))
  activity.name(built) |> should.equal(activities.enrich_brief_name)
}

pub fn enrich_brief_merges_and_writes_the_worktree_brief_in_place_test() {
  let document = enrich_document()
  let root = "/tmp/aion-stacked-dev-enrich-test/happy"
  let path = seed_brief(root, document)
  let report = scout_report([scout_entry("R1", "n1"), scout_entry("R2", "n2")])

  let assert Ok(returned) =
    locals.enrich_brief(EnrichInput(
      workspace: enrich_workspace(root),
      document: document,
      enrichment: ScoutEnrichment(report: report),
    ))

  // The return value is exactly the pure merge's result, and the file at the
  // derived design-system path now decodes as that document.
  let assert Ok(expected) = enrich.merge_scout(document, report)
  returned |> should.equal(expected)
  let brief_codec = codecs_brief.brief_document_codec()
  let assert Ok(on_disk_raw) = fixtures.read_file(path)
  let assert Ok(on_disk) = brief_codec.decode(on_disk_raw)
  on_disk |> should.equal(returned)

  // The on-disk authored subset encodes byte-identically to the pre-call one.
  brief_codec.encode(enrich.authored_subset(on_disk))
  |> should.equal(brief_codec.encode(enrich.authored_subset(document)))
}

pub fn enrich_brief_fails_terminally_on_authored_divergence_test() {
  let document = enrich_document()
  let root = "/tmp/aion-stacked-dev-enrich-test/divergent"
  // The on-disk brief carries a different authored task field.
  let path =
    seed_brief(root, BriefDocument(..document, task: "A different task."))
  let assert Ok(before) = fixtures.read_file(path)

  let assert Error(error.Terminal(message: message, details: _)) =
    locals.enrich_brief(EnrichInput(
      workspace: enrich_workspace(root),
      document: document,
      enrichment: ScoutEnrichment(report: scout_report([scout_entry("R1", "")])),
    ))

  // The failure names the divergent field and the brief path, and the file
  // bytes are untouched — divergence is never silently overwritten (CN3).
  message |> string.contains("task") |> should.be_true
  message |> string.contains(path) |> should.be_true
  let assert Ok(after) = fixtures.read_file(path)
  after |> should.equal(before)
}

pub fn enrich_brief_fails_terminally_when_the_brief_file_is_absent_test() {
  let document = enrich_document()
  let root = "/tmp/aion-stacked-dev-enrich-test/absent"
  let assert Ok(Nil) = remove_tree(root)

  let assert Error(error.Terminal(message: message, details: _)) =
    locals.enrich_brief(EnrichInput(
      workspace: enrich_workspace(root),
      document: document,
      enrichment: ScoutEnrichment(report: scout_report([scout_entry("R1", "")])),
    ))

  // A broken worktree is a can't-execute condition naming the derived path.
  message
  |> string.contains(root <> "/docs/design/brief-dev/briefs/BD-900.json")
  |> should.be_true
}

pub fn enrich_brief_records_gate_and_attestation_distinctly_test() {
  let document = enrich_document()
  let root = "/tmp/aion-stacked-dev-enrich-test/execution"
  let path = seed_brief(root, document)
  let block = sample_execution_block("wf-execution")

  let assert Ok(_) =
    locals.enrich_brief(EnrichInput(
      workspace: enrich_workspace(root),
      document: document,
      enrichment: ExecutionEnrichment(block: block),
    ))

  // The handed block was written exactly as given: the measured gate and the
  // believed attestation arrive on disk as two distinct objects (P1).
  let assert Ok(raw) = fixtures.read_file(path)
  raw |> string.contains("\"gate\"") |> should.be_true
  raw |> string.contains("\"attestation\"") |> should.be_true
  let assert Ok(on_disk) = codecs_brief.brief_document_codec().decode(raw)
  let assert option.Some(execution) = on_disk.execution
  execution.gate |> should.equal(block.gate)
  execution.attestation |> should.equal(block.attestation)
  execution |> should.equal(block)
}

pub fn enriched_fixture_is_emitted_for_the_design_system_validator_test() {
  // The seeded BD-001 brief merged through all four write points. The emitted
  // file is the merge functions' own output — never hand-written JSON — so
  // `python3 docs/design-system/scripts/validate.py
  // /tmp/aion-stacked-dev-enrich/briefs/enriched-fixture.json` (run from the
  // repo root after `gleam test`) proves the enriched document still
  // validates against brief.schema.json.
  let assert Ok(raw) = fixtures.read_file(enrich_fixture_path)
  let brief_codec = codecs_brief.brief_document_codec()
  let assert Ok(document) = brief_codec.decode(raw)
  let ids = list.map(document.requirements, fn(requirement) { requirement.id })

  let assert Ok(scouted) =
    enrich.merge_scout(
      document,
      scout_report(
        list.map(ids, fn(id) { scout_entry(id, "fixture notes for " <> id) }),
      ),
    )
  let assert Ok(developed) =
    enrich.merge_dev(scouted, dev_report(list.map(ids, dev_entry)))
  let assert Ok(reviewed) =
    enrich.merge_review(developed, review_report(list.map(ids, review_entry)))
  let assert Ok(enriched) =
    enrich.merge_execution(reviewed, sample_execution_block("wf-bd001-fixture"))

  // Every requirement carries all three stage blocks; the execution block is
  // present with its gate and attestation as distinct objects; the authored
  // subset is byte-stable through the full enrichment.
  list.all(enriched.requirements, fn(requirement) {
    requirement.scout != option.None
    && requirement.dev != option.None
    && requirement.review != option.None
  })
  |> should.be_true
  let assert option.Some(execution) = enriched.execution
  execution.gate
  |> should.equal(GateBlock(fmt: True, clippy: True, tests: True, fix_rounds: 0))
  execution.attestation
  |> should.equal(AttestationBlock(
    no_panics: True,
    no_unsafe: True,
    boundaries_respected: True,
    tests_pass: True,
  ))
  brief_codec.encode(enrich.authored_subset(enriched))
  |> should.equal(brief_codec.encode(enrich.authored_subset(document)))

  // Emit under a fresh briefs/ directory — the parent directory name is what
  // validate.py keys its brief-schema detection on.
  let assert Ok(Nil) = remove_tree(enriched_fixture_dir)
  let assert Ok(Nil) =
    write_file(enriched_fixture_file, brief_codec.encode(enriched))
}
