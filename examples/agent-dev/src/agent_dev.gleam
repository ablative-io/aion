//// The agent-dev workflow: brief in, reviewed-and-gated-and-landed out.
////
//// The Phase-2 NOI dogfood pipeline. Pure orchestration over six recorded
//// activities:
////
////   provision -> scout -> dev -> review
////   while !review.pass:  dev(feedback); review(new report)      [bounded]
////   gate
////   while !gate.pass:    dev(diagnostics); [inner review loop]; gate
////   land                                                    [Passed only]
////
//// The dev<->review loop is bounded by `dev_review_cap` (cumulative across
//// the whole run, including the inner loops that gate failures re-enter);
//// the gate loop by `gate_cap`. Exhausting a cap is a terminal DISPOSITION
//// (`ReviewCapExhausted` / `GateCapExhausted`) carried in the output, never
//// an error — and `land` is skipped on exhausted dispositions, leaving the
//// workspace intact for inspection.
////
//// scout/dev/review are agent activities under the norn-harness contract:
//// one prompt string in, one terminal-text string out. This workflow
//// composes every prompt (`agent_dev/prompts`) and decodes the review
//// verdict defensively (`agent_dev/verdict`) with ONE bounded re-ask round
//// for an unparseable reply; a still-unparseable reply counts as a failed
//// review round.
////
//// An `agent_dev_status` query answers `{phase, round}` live state; the
//// handler is re-registered at every stage transition, so replay re-arms it
//// automatically.
////
//// This module is the determinism boundary: it issues only recorded
//// activity dispatches and branches on their recorded outputs. No wall
//// clock, no entropy, no direct IO.

import agent_dev/activities
import agent_dev/prompts
import agent_dev/verdict
import agent_dev_codecs as codecs
import agent_dev_io as io
import aion/activity
import aion/error
import aion/query
import aion/workflow
import gleam/dynamic.{type Dynamic}

/// Name of the live `{phase, round}` status query this workflow answers.
pub const status_query_name = "agent_dev_status"

/// Typed definition binding the generated codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  io.Input,
  io.Output,
  io.AgentDevError,
) {
  workflow.define(
    "agent_dev",
    codecs.input_codec(),
    codecs.output_codec(),
    codecs.agent_dev_error_codec(),
    execute,
  )
}

/// Engine entry point.
///
/// The runtime delivers the start input as a raw JSON string;
/// `workflow.entrypoint` decodes it with the definition's input codec,
/// drives `execute`, and encodes the outcome back to JSON text. An
/// undecodable input records the SDK's documented
/// `{"aion_error":"input_decode",...}` envelope as the failure payload.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  workflow.entrypoint(definition(), raw_input)
}

/// Typed workflow body: provision, scout, first dev round, the bounded
/// dev<->review loop, the bounded gate loop, then land — on Passed only.
pub fn execute(input: io.Input) -> Result(io.Output, io.AgentDevError) {
  use _ <- result_try(set_status("provisioning", 0))
  use workspace <- result_try(run_provision(input))
  use _ <- result_try(set_status("scouting", 0))
  use plan <- result_try(run_agent(
    activities.scout,
    "scout",
    prompts.scout(input),
  ))
  use _ <- result_try(set_status("developing", 1))
  use dev_report <- result_try(run_agent(
    activities.dev,
    "dev",
    prompts.dev_start(input, plan),
  ))

  // The dev<->review loop. The first entry is at round 0, so with
  // `dev_review_cap >= 1` the placeholder fallback verdict is never
  // surfaced — it only matters on a gate re-entry already at the cap.
  use loop <- result_try(review_loop(
    input,
    dev_report,
    False,
    0,
    not_yet_reviewed(),
  ))
  case loop.passed {
    // The dev<->review budget is spent before any gate ran: terminate as
    // review_cap_exhausted with an honest "gate not run" detail.
    False ->
      Ok(build_output(
        io.ReviewCapExhausted,
        loop.review,
        gate_not_run(),
        loop.dev_review_rounds,
        0,
        workspace,
      ))
    True -> gate_loop(input, workspace, loop, 0)
  }
}

// --- the dev<->review loop ---------------------------------------------------

/// The state a bounded dev<->review loop run terminates with: the latest
/// verdict, the cumulative round count, whether the review session exists
/// (so the next review prompt is a lean resume), and whether the loop
/// converged on a passing review (vs. spent its cap).
type ReviewLoop {
  ReviewLoop(
    review: io.ReviewVerdict,
    dev_review_rounds: Int,
    review_started: Bool,
    passed: Bool,
  )
}

/// Run review rounds until the review passes or the cumulative dev<->review
/// budget (`dev_review_cap`) is spent. Each non-passing round resumes the
/// dev session with the blockers, then re-reviews the new report.
///
/// The cap is checked BEFORE each review so the budget is never overrun:
/// when `rounds_so_far` has already reached the cap on entry (only reachable
/// on a gate-driven re-entry), no further review runs and the loop
/// terminates as exhausted carrying `fallback_review` (the last verdict that
/// actually ran), keeping the cap a true cumulative ceiling.
fn review_loop(
  input: io.Input,
  dev_report: String,
  review_started: Bool,
  rounds_so_far: Int,
  fallback_review: io.ReviewVerdict,
) -> Result(ReviewLoop, io.AgentDevError) {
  case rounds_so_far >= input.dev_review_cap {
    True ->
      Ok(ReviewLoop(
        review: fallback_review,
        dev_review_rounds: rounds_so_far,
        review_started: review_started,
        passed: False,
      ))
    False -> {
      use _ <- result_try(set_status("reviewing", rounds_so_far + 1))
      use review_verdict <- result_try(run_review(
        input,
        dev_report,
        review_started,
      ))
      let rounds = rounds_so_far + 1
      case review_verdict.pass {
        True ->
          Ok(ReviewLoop(
            review: review_verdict,
            dev_review_rounds: rounds,
            review_started: True,
            passed: True,
          ))
        False ->
          case rounds >= input.dev_review_cap {
            True ->
              Ok(ReviewLoop(
                review: review_verdict,
                dev_review_rounds: rounds,
                review_started: True,
                passed: False,
              ))
            False -> {
              use _ <- result_try(set_status("developing", rounds + 1))
              use revised <- result_try(run_agent(
                activities.dev,
                "dev",
                prompts.dev_review_feedback(review_verdict),
              ))
              review_loop(input, revised, True, rounds, review_verdict)
            }
          }
      }
    }
  }
}

/// One review dispatch plus the defensive verdict decode: parse the trailing
/// JSON object out of the terminal text; on an unparseable reply, ONE
/// bounded re-ask ("respond with only the JSON verdict"); still unparseable
/// counts as a failed review round with an honest verdict saying so.
fn run_review(
  input: io.Input,
  dev_report: String,
  review_started: Bool,
) -> Result(io.ReviewVerdict, io.AgentDevError) {
  let prompt = case review_started {
    False -> prompts.review_start(input, dev_report)
    True -> prompts.review_resume(dev_report)
  }
  use reply <- result_try(run_agent(activities.review, "review", prompt))
  case verdict.parse(reply) {
    Ok(parsed) -> Ok(parsed)
    Error(Nil) -> {
      use reasked <- result_try(run_agent(
        activities.review,
        "review",
        prompts.verdict_reask,
      ))
      case verdict.parse(reasked) {
        Ok(parsed) -> Ok(parsed)
        Error(Nil) -> Ok(unparseable_verdict())
      }
    }
  }
}

// --- the gate loop -----------------------------------------------------------

/// Run the gate, then while it fails and the gate budget (`gate_cap`)
/// remains, resume the dev session with the diagnostics, re-enter the
/// bounded dev<->review loop (cumulative cap), and re-gate. A passing gate
/// lands and finishes as `Passed`; a spent gate budget finishes as
/// `GateCapExhausted`; a spent dev<->review budget inside a re-entry
/// finishes as `ReviewCapExhausted`. Exhausted dispositions never land.
fn gate_loop(
  input: io.Input,
  workspace: io.Workspace,
  loop: ReviewLoop,
  gate_rounds_so_far: Int,
) -> Result(io.Output, io.AgentDevError) {
  use _ <- result_try(set_status("gating", gate_rounds_so_far + 1))
  use gate_detail <- result_try(run_gate(workspace))
  let gate_rounds = gate_rounds_so_far + 1
  case gate_detail.pass {
    True -> land_and_finish(input, workspace, loop, gate_detail, gate_rounds)
    False ->
      case gate_rounds >= input.gate_cap {
        True ->
          Ok(build_output(
            io.GateCapExhausted,
            loop.review,
            gate_detail,
            loop.dev_review_rounds,
            gate_rounds,
            workspace,
          ))
        False -> {
          use _ <- result_try(set_status(
            "developing",
            loop.dev_review_rounds + 1,
          ))
          use revised <- result_try(run_agent(
            activities.dev,
            "dev",
            prompts.dev_gate_feedback(gate_detail.diagnostics),
          ))
          use inner <- result_try(review_loop(
            input,
            revised,
            loop.review_started,
            loop.dev_review_rounds,
            loop.review,
          ))
          case inner.passed {
            False ->
              Ok(build_output(
                io.ReviewCapExhausted,
                inner.review,
                gate_detail,
                inner.dev_review_rounds,
                gate_rounds,
                workspace,
              ))
            True -> gate_loop(input, workspace, inner, gate_rounds)
          }
        }
      }
  }
}

/// The Passed terminal: land the branch, then build the output. `land` runs
/// here and ONLY here.
fn land_and_finish(
  input: io.Input,
  workspace: io.Workspace,
  loop: ReviewLoop,
  gate_detail: io.GateDetail,
  gate_rounds: Int,
) -> Result(io.Output, io.AgentDevError) {
  use _ <- result_try(set_status("landing", gate_rounds))
  use _landed <- result_try(run_land(workspace, input.brief_id))
  Ok(build_output(
    io.Passed,
    loop.review,
    gate_detail,
    loop.dev_review_rounds,
    gate_rounds,
    workspace,
  ))
}

// --- activity dispatches -----------------------------------------------------

fn run_provision(input: io.Input) -> Result(io.Workspace, io.AgentDevError) {
  case
    workflow.run(
      activities.provision(io.ProvisionInput(
        repo_url: input.repo_url,
        base_ref: input.base_ref,
        brief_id: input.brief_id,
      )),
    )
  {
    Ok(workspace) -> Ok(workspace)
    Error(activity_error) -> stage_error("provision", activity_error)
  }
}

/// Dispatch one agent step (scout/dev/review): prompt in, terminal text
/// out. `step` is the typed activity constructor from `agent_dev/activities`;
/// `stage` names the step in a typed error.
fn run_agent(
  step: fn(String) -> activity.Activity(String, String),
  stage: String,
  prompt: String,
) -> Result(String, io.AgentDevError) {
  case workflow.run(step(prompt)) {
    Ok(reply) -> Ok(reply)
    Error(activity_error) -> stage_error(stage, activity_error)
  }
}

fn run_gate(
  workspace: io.Workspace,
) -> Result(io.GateDetail, io.AgentDevError) {
  case workflow.run(activities.gate(workspace)) {
    Ok(gate_detail) -> Ok(gate_detail)
    Error(activity_error) -> stage_error("gate", activity_error)
  }
}

fn run_land(
  workspace: io.Workspace,
  brief_id: String,
) -> Result(io.LandOutput, io.AgentDevError) {
  case
    workflow.run(
      activities.land(io.LandInput(workspace: workspace, brief_id: brief_id)),
    )
  {
    Ok(landed) -> Ok(landed)
    Error(activity_error) -> stage_error("land", activity_error)
  }
}

// --- status query ------------------------------------------------------------

/// Re-register the status handler with the current phase and round, so
/// `agent_dev_status` queries answer live state at every yield point
/// (re-registration per stage, per docs/guides/workflows.md).
fn set_status(phase: String, round: Int) -> Result(Nil, io.AgentDevError) {
  let status = io.AgentDevStatus(phase: phase, round: round)
  case
    query.handler(status_query_name, codecs.agent_dev_status_codec(), fn() {
      status
    })
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) ->
      Error(io.AgentDevError(
        stage: "register_status",
        message: query_error_message(query_error),
      ))
  }
}

// --- helpers -----------------------------------------------------------------

fn build_output(
  disposition: io.Disposition,
  last_review: io.ReviewVerdict,
  gate_detail: io.GateDetail,
  dev_review_rounds: Int,
  gate_rounds: Int,
  workspace: io.Workspace,
) -> io.Output {
  io.Output(
    disposition: disposition,
    dev_review_rounds: dev_review_rounds,
    gate_rounds: gate_rounds,
    last_review: last_review,
    gate_detail: gate_detail,
    branch: workspace.branch,
    workspace_path: workspace.path,
  )
}

/// The gate detail carried when the gate never ran (a review_cap_exhausted
/// run that stopped before any gate). `pass: False` with empty diagnostics
/// records "not run" honestly, never a fake pass.
fn gate_not_run() -> io.GateDetail {
  io.GateDetail(pass: False, diagnostics: "")
}

/// The placeholder fallback for the first `review_loop` entry. Never
/// surfaced: the first entry is at round 0 with `dev_review_cap >= 1`, so a
/// real review always runs before any exhaustion. `pass: False` keeps it
/// honest if it ever were observed.
fn not_yet_reviewed() -> io.ReviewVerdict {
  io.ReviewVerdict(pass: False, blockers: [], summary: "no review has run yet")
}

/// The honest verdict recorded when the reviewer's reply carried no
/// parseable JSON object even after the one bounded re-ask: a failed review
/// round, never an invented pass and never a workflow error.
fn unparseable_verdict() -> io.ReviewVerdict {
  io.ReviewVerdict(
    pass: False,
    blockers: [
      "the reviewer did not return a parseable JSON verdict after one re-ask",
    ],
    summary: "review verdict unparseable; counted as a failed review round",
  )
}

fn stage_error(
  stage: String,
  activity_error: error.ActivityError,
) -> Result(value, io.AgentDevError) {
  Error(io.AgentDevError(
    stage: stage,
    message: activity_error_message(activity_error),
  ))
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}

fn query_error_message(query_error: error.QueryError) -> String {
  case query_error {
    error.UnknownQuery(name: name) -> "unknown query: " <> name
    error.QueryDecodeFailed(_) -> "query reply could not be decoded"
    error.QueryTimedOut(error.TimedOut(message: message)) -> message
    error.QueryNotRunning(workflow_id: workflow_id) ->
      "query target not running: " <> workflow_id
    error.QueryHandlerFailed(message: message) -> message
    error.QueryEngineFailure(message) -> message
  }
}

fn result_try(
  result: Result(value, io.AgentDevError),
  next: fn(value) -> Result(output, io.AgentDevError),
) -> Result(output, io.AgentDevError) {
  case result {
    Ok(value) -> next(value)
    Error(workflow_error) -> Error(workflow_error)
  }
}
