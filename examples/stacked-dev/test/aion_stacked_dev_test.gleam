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

import aion/query
import aion/signal
import aion/testing
import gleam/string
import gleeunit
import gleeunit/should
import onatopp_dev
import stacked_dev
import stacked_dev/codecs_workflows
import stacked_dev/types.{
  type ReviewVerdict, type StackedDevInput, Approve, GateRejected, Local,
  OnatoppStatus, ProvisionFailed, Reject, RequestChanges, ReviewCapExhausted,
  ReviewNote, ReviewRejected, ReviewTimedOut, ReviewVerdict, StackedDevInput,
  StackedDevStatus, VerifyExhausted, Worktree,
}
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
    brief: "Implement the stacked-dev example",
    design: "docs/design.md",
    checklist: "docs/checklist.md",
    stories: ["story-1", "story-2"],
    verify_fix_cap: 3,
    review_cap: 3,
    round_backoff_ms: 25,
    review_deadline_ms: 60_000,
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

  // Land is the yg-level stack operation: merge the branch into its parent,
  // exactly once, after review approved.
  shims.invocations(shim_set, "yg", "branch merge " <> shims.landed_branch)
  |> should.equal(1)
  // The review request carried the reviewer flags and the branch.
  shims.log(shim_set, "meridian")
  |> string.contains(
    "review request --reviewer sample-reviewer " <> shims.landed_branch,
  )
  |> should.be_true

  // The startup fan-out really warmed the cache concurrently with dev.
  shims.invocations(shim_set, "cargo", "build") |> should.equal(1)
  shims.invocations(shim_set, "norn", "--print --session-id")
  |> should.equal(1)
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
    onatopp_dev.status_query_name,
    codecs_workflows.onatopp_status_codec(),
  )
  |> should.equal(Ok(OnatoppStatus(phase: "verifying", round: 2)))
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
    onatopp_dev.status_query_name,
    codecs_workflows.onatopp_status_codec(),
  )
  |> should.equal(Ok(OnatoppStatus(phase: "converged", round: 1)))

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
