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
  type ReviewVerdict, type StackedDevInput, Approve, Local, OnatoppStatus,
  ProvisionFailed, Reject, RequestChanges, ReviewNote, ReviewRejected,
  ReviewTimedOut, ReviewVerdict, StackedDevInput, StackedDevStatus,
  VerifyExhausted, Worktree,
}
import support/shims

pub fn main() {
  gleeunit.main()
}

/// Workflow input used by every scenario. Caps, backoff, and deadline are
/// required fields (open question Q5), so each test states them explicitly.
fn base_input() -> StackedDevInput {
  StackedDevInput(
    brief_id: "brief-7",
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

/// Fresh harness env + shim dir with the full pipeline (real local impls
/// and real children) registered. The cargo shim is scenario-specific, so
/// callers install it themselves.
fn pipeline(
  install_cargo: fn(shims.Shims) -> Nil,
) -> #(testing.TestEnv, shims.Shims) {
  let #(env, shim_set) = bare_pipeline()
  shims.write_meridian(shim_set)
  shims.write_norn(shim_set)
  install_cargo(shim_set)
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

fn send_verdict(verdict: ReviewVerdict) -> Nil {
  let assert Ok(Nil) =
    signal.send("stacked-dev-test-run", stacked_dev.review_signal(), verdict)
  Nil
}

pub fn full_pipeline_happy_path_approves_first_round_test() {
  let #(_env, shim_set) = pipeline(shims.write_cargo_passing)
  send_verdict(ReviewVerdict(decision: Approve))

  let assert Ok(result) = stacked_dev.execute(base_input())

  result.pr_url |> should.equal(shims.pr_url)
  result.merge_commit |> should.equal(shims.merge_commit)
  result.session_id |> should.equal(shims.session_id)
  result.build_warm.ok |> should.be_true
  result.verify_rounds |> should.equal(1)
  result.review_rounds |> should.equal(1)

  // The provision shim was driven with the typed placement/isolation axis.
  shims.log(shim_set, "meridian")
  |> string.contains("--isolation worktree --placement local")
  |> should.be_true

  // Land really means stack submit THEN stack land, exactly once each.
  shims.invocations(shim_set, "meridian", "stack submit")
  |> should.equal(1)
  shims.invocations(shim_set, "meridian", "stack land")
  |> should.equal(1)
  let assert Ok(#(_, after_submit)) =
    string.split_once(shims.log(shim_set, "meridian"), "stack submit")
  after_submit |> string.contains("stack land") |> should.be_true

  // The startup fan-out really warmed the cache concurrently with dev.
  shims.invocations(shim_set, "cargo", "build") |> should.equal(1)
  shims.invocations(shim_set, "norn", "run") |> should.equal(1)
}

pub fn verify_fix_loop_converges_on_round_two_test() {
  let #(_env, shim_set) =
    pipeline(fn(shim_set) {
      shims.write_cargo_failing_scoped_clippy(shim_set, 1)
    })
  send_verdict(ReviewVerdict(decision: Approve))

  let assert Ok(result) = stacked_dev.execute(base_input())

  // Round 1 failed scoped clippy, dev_resume fed the diagnostics back, and
  // round 2 converged.
  result.verify_rounds |> should.equal(2)
  result.review_rounds |> should.equal(1)
  shims.invocations(shim_set, "cargo", "clippy -p aion-core")
  |> should.equal(2)

  // The scoped-check diagnostics reached the resumed agent's argv intact.
  let norn_log = shims.log(shim_set, "norn")
  norn_log
  |> string.contains("resume --session " <> shims.session_id)
  |> should.be_true
  norn_log |> string.contains(shims.clippy_diagnostics) |> should.be_true
}

pub fn verify_fix_exhaustion_surfaces_typed_diagnostics_test() {
  let #(_env, shim_set) =
    pipeline(fn(shim_set) {
      // Scoped clippy never passes.
      shims.write_cargo_failing_scoped_clippy(shim_set, 1_000_000)
    })

  let input = StackedDevInput(..base_input(), verify_fix_cap: 2)
  let assert Error(VerifyExhausted(rounds: rounds, diagnostics: diagnostics)) =
    stacked_dev.execute(input)

  rounds |> should.equal(2)
  diagnostics |> string.contains(shims.clippy_diagnostics) |> should.be_true
  diagnostics |> string.contains("clippy -p aion-core") |> should.be_true

  // The run never reached the gate, review, or land stages.
  shims.invocations(shim_set, "cargo", "clippy --workspace")
  |> should.equal(0)
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(0)
  shims.invocations(shim_set, "meridian", "stack submit") |> should.equal(0)

  // The child's status query reports where it stopped: still verifying at
  // the capped round.
  query.dispatch(
    onatopp_dev.status_query_name,
    codecs_workflows.onatopp_status_codec(),
  )
  |> should.equal(Ok(OnatoppStatus(phase: "verifying", round: 2)))
}

pub fn review_request_changes_notes_reach_dev_resume_and_regate_test() {
  let #(_env, shim_set) = pipeline(shims.write_cargo_passing)
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

  let assert Ok(result) = stacked_dev.execute(base_input())

  result.review_rounds |> should.equal(2)
  result.verify_rounds |> should.equal(1)

  // The structured notes (open question Q3) reached the resumed agent's
  // argv as data: file, line, and note all present in the recorded feedback.
  let norn_log = shims.log(shim_set, "norn")
  norn_log
  |> string.contains("resume --session " <> shims.session_id)
  |> should.be_true
  norn_log |> string.contains("crates/aion-core/src/lib.rs") |> should.be_true
  norn_log |> string.contains("\"line\":42") |> should.be_true
  norn_log |> string.contains("tighten the error taxonomy") |> should.be_true

  // Each fix round re-gates: the workspace-wide gate ran twice, the review
  // was requested twice, and the stack landed once.
  shims.invocations(shim_set, "cargo", "clippy --workspace")
  |> should.equal(2)
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(2)
  shims.invocations(shim_set, "meridian", "stack land")
  |> should.equal(1)
}

pub fn review_reject_fails_the_run_with_typed_reason_test() {
  let #(_env, shim_set) = pipeline(shims.write_cargo_passing)
  send_verdict(ReviewVerdict(decision: Reject(reason: "wrong architecture")))

  stacked_dev.execute(base_input())
  |> should.equal(Error(ReviewRejected(reason: "wrong architecture")))

  // A rejected run never submits or lands.
  shims.invocations(shim_set, "meridian", "stack submit") |> should.equal(0)
  shims.invocations(shim_set, "meridian", "stack land") |> should.equal(0)
}

pub fn review_timeout_fails_the_run_with_typed_deadline_test() {
  let #(_env, shim_set) = pipeline(shims.write_cargo_passing)
  // No verdict is ever sent; the durable deadline expires instead.
  let input = StackedDevInput(..base_input(), review_deadline_ms: 0)

  stacked_dev.execute(input)
  |> should.equal(Error(ReviewTimedOut(deadline_ms: 0)))

  // The review was requested, but nothing was submitted or landed.
  shims.invocations(shim_set, "meridian", "review request")
  |> should.equal(1)
  shims.invocations(shim_set, "meridian", "stack submit") |> should.equal(0)
  shims.invocations(shim_set, "meridian", "stack land") |> should.equal(0)
}

pub fn warm_build_failure_is_advisory_and_never_fails_the_run_test() {
  let #(_env, shim_set) = pipeline(shims.write_cargo_failing_build)
  send_verdict(ReviewVerdict(decision: Approve))

  let assert Ok(result) = stacked_dev.execute(base_input())

  // The forfeited cache is recorded as advisory data; the run still landed.
  result.build_warm.ok |> should.be_false
  result.pr_url |> should.equal(shims.pr_url)
  result.review_rounds |> should.equal(1)
  shims.invocations(shim_set, "cargo", "build") |> should.equal(1)
}

pub fn status_query_answers_live_phase_and_round_per_stage_test() {
  // A landed run reports the terminal phase with its review round, and the
  // child reports its converged verify round.
  let #(_env, _shim_set) = pipeline(shims.write_cargo_passing)
  send_verdict(ReviewVerdict(decision: Approve))
  let assert Ok(_) = stacked_dev.execute(base_input())
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
  let #(_env, _shim_set) = pipeline(shims.write_cargo_passing)
  send_verdict(ReviewVerdict(decision: Reject(reason: "no")))
  let assert Error(ReviewRejected(reason: "no")) =
    stacked_dev.execute(base_input())
  query.dispatch(
    stacked_dev.status_query_name,
    codecs_workflows.stacked_dev_status_codec(),
  )
  |> should.equal(Ok(StackedDevStatus(phase: "in_review", round: 1)))
}

pub fn missing_cli_with_no_shim_is_a_loud_activity_failure_test() {
  // PATH points at an empty shim directory: no meridian, no norn, no cargo.
  // The very first activity must fail loudly, naming the absent executable
  // — activities are never silently skipped.
  let #(_env, _shim_set) = bare_pipeline()

  let assert Error(ProvisionFailed(message: message)) =
    stacked_dev.execute(base_input())
  message
  |> string.contains("executable not found on PATH: meridian")
  |> should.be_true
}
