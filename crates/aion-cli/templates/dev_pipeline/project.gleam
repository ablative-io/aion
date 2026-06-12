//// The top-level pipeline workflow: brief in, landed on main out.
////
//// Control flow (brief section 5):
////
//// 1. `provision_workspace` — everything downstream needs the `Workspace`.
//// 2. the dev child `{{name}}_dev` (`workflow.spawn_and_wait`): concurrent
////    warm-build + dev via `workflow.all`, then the bounded scoped
////    verify-fix loop.
//// 3. `gate` child (`workflow.spawn_and_wait`): the authoritative
////    workspace-wide checks, run once after the verify loop converges.
//// 4. The bounded review loop: `request_review`, then `workflow.receive`
////    on the `review_verdict` signal raced against a durable deadline with
////    `workflow.with_timeout`. Approve proceeds; RequestChanges resumes the
////    dev session with the structured notes, re-gates, and re-requests;
////    Reject or a deadline expiry is a typed `Failed`.
//// 5. `land` — `yg branch merge` into the tree parent, only on Approve and
////    a passing gate.
////
//// A `{{name}}_status` query answers `{phase, round}` live state; the
//// handler is re-registered at every stage transition, so replay re-arms it
//// automatically.
////
//// Resolves open question Q6 (one workflow or a family): all three
//// workflows are independently dispatchable entries of this one package,
//// AND this top-level composes the two children via `spawn_and_wait`.
//// Every loop cap, backoff, and deadline is a REQUIRED input field — no
//// arbitrary defaults baked in (open question Q5).

import aion/codec
import aion/duration
import aion/error
import aion/query
import aion/signal
import aion/workflow
import {{name}}_gate
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import {{name}}_dev
import {{name}}/activities
import {{name}}/codecs_flow
import {{name}}/codecs_workflows
import {{name}}/errors
import {{name}}/types.{
  type BuildWarm, type DevResult, type GateResult, type ReviewVerdict,
  type PipelineError, type PipelineInput, type PipelineResult,
  type Workspace, Approve, DevFailed, GateFail, GateInput, GatePass,
  GateRejected, GateResult, LandFailed, LandInput, DevFlowInput,
  DevFlowStageFailed, ProvisionFailed, ProvisionInput, Reject, RequestChanges,
  ResumeInput, ReviewCapExhausted, ReviewRejected, ReviewRequest, ReviewTimedOut,
  ReviewVerdict, PipelineResult, PipelineStatus, StageFailed, StartupFailed,
  VerifyExhausted, VerifyFixExhausted, WorkspaceWide,
}

/// Name of the human/SDK review-verdict signal this workflow waits on.
/// Drive it with:
/// `aion signal <run-id> review_verdict --payload '{"decision":"approve"}'`.
pub const review_signal_name = "review_verdict"

/// Name of the live `{phase, round}` status query this workflow answers.
pub const status_query_name = "{{name}}_status"

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  PipelineInput,
  PipelineResult,
  PipelineError,
) {
  workflow.define(
    "{{name}}",
    codecs_workflows.pipeline_input_codec(),
    codecs_workflows.pipeline_result_codec(),
    codecs_workflows.pipeline_error_codec(),
    execute,
  )
}

/// Typed reference to the review-verdict signal (also used by tests and
/// in-engine senders).
pub fn review_signal() -> workflow.SignalRef(ReviewVerdict) {
  signal.new(review_signal_name, codecs_flow.review_verdict_codec())
}

/// Engine entry point.
///
/// The runtime delivers the start input as a raw JSON string: decode it with
/// the input codec, run the typed workflow, and encode the success value
/// back to its JSON string for the recorded result payload.
pub fn run(raw_input: Dynamic) -> Result(String, PipelineError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs_workflows.pipeline_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) ->
              Ok(codecs_workflows.pipeline_result_codec().encode(output))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(StageFailed(
            stage: "decode_input",
            message: "failed to decode workflow input: " <> reason,
          ))
      }
    Error(_) ->
      Error(StageFailed(
        stage: "decode_input",
        message: "workflow input payload was not a string",
      ))
  }
}

/// Typed workflow body: provision, dev child, gate child, review loop, land.
pub fn execute(
  input: PipelineInput,
) -> Result(PipelineResult, PipelineError) {
  use _ <- result_try(set_status("provisioning", 0))
  use workspace <- result_try(provision(input))
  use _ <- result_try(set_status("developing", 0))
  use dev_flow <- result_try(run_dev_flow(input, workspace))
  use _ <- result_try(set_status("gating", 0))
  use gate_result <- result_try(run_gate(
    workspace,
    dev_flow.dev_result.files_touched,
  ))
  case gate_result {
    GateResult(verdict: GatePass) ->
      review_loop(
        input,
        workspace,
        dev_flow.dev_result,
        gate_result,
        dev_flow.build_warm,
        dev_flow.verify_rounds,
        1,
      )
    GateResult(verdict: GateFail(report: report)) ->
      // A converged verify loop that still fails the authoritative gate
      // surfaces loudly instead of silently looping: scoped checks missed
      // something and the report says what.
      Error(GateRejected(report: report))
  }
}

fn provision(input: PipelineInput) -> Result(Workspace, PipelineError) {
  case
    workflow.run(
      activities.provision_workspace(ProvisionInput(
        repo_root: input.repo_root,
        brief_id: input.brief_id,
        base_ref: input.base_ref,
        placement: input.placement,
        isolation: input.isolation,
      )),
    )
  {
    Ok(workspace) -> Ok(workspace)
    Error(activity_error) ->
      Error(ProvisionFailed(message: errors.activity_message(activity_error)))
  }
}

/// Spawn the `{{name}}_dev` child and lift its typed errors into this
/// workflow's error union — exhaustion keeps its rounds and diagnostics.
fn run_dev_flow(
  input: PipelineInput,
  workspace: Workspace,
) -> Result(types.DevFlowResult, PipelineError) {
  case
    workflow.spawn_and_wait(
      {{name}}_dev.workflow_type,
      {{name}}_dev.execute,
      DevFlowInput(
        workspace: workspace,
        brief: input.brief,
        design: input.design,
        checklist: input.checklist,
        stories: input.stories,
        verify_fix_cap: input.verify_fix_cap,
        round_backoff_ms: input.round_backoff_ms,
      ),
      codecs_workflows.dev_flow_input_codec(),
      codecs_workflows.dev_flow_result_codec(),
      codecs_workflows.dev_flow_error_codec(),
    )
  {
    Ok(result) -> Ok(result)
    Error(error.ChildWorkflowFailed(VerifyFixExhausted(
      rounds: rounds,
      diagnostics: diagnostics,
    ))) -> Error(VerifyExhausted(rounds: rounds, diagnostics: diagnostics))
    Error(error.ChildWorkflowFailed(StartupFailed(message: message))) ->
      Error(DevFailed(message: "startup failed: " <> message))
    Error(error.ChildWorkflowFailed(DevFlowStageFailed(
      stage: stage,
      message: message,
    ))) -> Error(DevFailed(message: stage <> ": " <> message))
    Error(child_error) ->
      Error(StageFailed(
        stage: "{{name}}_dev",
        message: child_engine_message(child_error),
      ))
  }
}

/// Spawn the `gate` child for the workspace-wide authoritative checks
/// (open question Q2: workspace-wide today; the affected-closure scope is a
/// typed seam).
fn run_gate(
  workspace: Workspace,
  files_touched: List(String),
) -> Result(GateResult, PipelineError) {
  case
    workflow.spawn_and_wait(
      {{name}}_gate.workflow_type,
      {{name}}_gate.execute,
      GateInput(
        workspace: workspace,
        files_touched: files_touched,
        scope: WorkspaceWide,
      ),
      codecs_workflows.gate_input_codec(),
      codecs_workflows.gate_result_codec(),
      codecs_workflows.gate_error_codec(),
    )
  {
    Ok(result) -> Ok(result)
    Error(error.ChildWorkflowFailed(types.GateStageFailed(
      stage: stage,
      message: message,
    ))) -> Error(StageFailed(stage: "gate/" <> stage, message: message))
    Error(child_error) ->
      Error(StageFailed(
        stage: "gate",
        message: child_engine_message(child_error),
      ))
  }
}

/// One bounded review round: request review, race the verdict signal
/// against the durable deadline, and act on the typed decision.
fn review_loop(
  input: PipelineInput,
  workspace: Workspace,
  dev_result: DevResult,
  gate_result: GateResult,
  build_warm: BuildWarm,
  verify_rounds: Int,
  round: Int,
) -> Result(PipelineResult, PipelineError) {
  case round > input.review_cap {
    True -> Error(ReviewCapExhausted(rounds: input.review_cap))
    False -> {
      use _ <- result_try(set_status("in_review", round))
      use _ <- result_try(request_review(
        input,
        workspace,
        dev_result,
        gate_result,
      ))
      case
        workflow.with_timeout(
          fn() { workflow.receive(review_signal()) },
          duration.milliseconds(input.review_deadline_ms),
        )
      {
        Ok(ReviewVerdict(decision: Approve)) ->
          land(
            workspace,
            input.repo_root,
            input.base_ref,
            dev_result,
            build_warm,
            verify_rounds,
            round,
          )
        Ok(ReviewVerdict(decision: RequestChanges(notes: notes))) ->
          fix_and_regate(
            input,
            workspace,
            dev_result,
            build_warm,
            verify_rounds,
            round,
            codecs_flow.review_notes_feedback(notes),
          )
        Ok(ReviewVerdict(decision: Reject(reason: reason))) ->
          Error(ReviewRejected(reason: reason))
        Error(error.TimedOutError(error.TimedOut(message: _))) ->
          Error(ReviewTimedOut(deadline_ms: input.review_deadline_ms))
        Error(error.InnerError(receive_error)) ->
          Error(StageFailed(
            stage: "await_verdict",
            message: errors.receive_message(receive_error),
          ))
        Error(error.TimeoutEngineFailure(message: message)) ->
          Error(StageFailed(stage: "await_verdict", message: message))
      }
    }
  }
}

fn request_review(
  input: PipelineInput,
  workspace: Workspace,
  dev_result: DevResult,
  gate_result: GateResult,
) -> Result(Nil, PipelineError) {
  case
    workflow.run(
      activities.request_review(ReviewRequest(
        workspace: workspace,
        brief_id: input.brief_id,
        reviewers: input.reviewers,
        dev_result: dev_result,
        gate_result: gate_result,
      )),
    )
  {
    Ok(_ack) -> Ok(Nil)
    Error(activity_error) ->
      Error(StageFailed(
        stage: "request_review",
        message: errors.activity_message(activity_error),
      ))
  }
}

/// RequestChanges path: resume the dev session with the structured notes,
/// re-gate, sleep the durable backoff, and enter the next review round.
fn fix_and_regate(
  input: PipelineInput,
  workspace: Workspace,
  dev_result: DevResult,
  build_warm: BuildWarm,
  verify_rounds: Int,
  round: Int,
  feedback: String,
) -> Result(PipelineResult, PipelineError) {
  case
    workflow.run(
      activities.dev_resume(ResumeInput(
        session_id: dev_result.session_id,
        feedback: feedback,
      )),
    )
  {
    Ok(resumed) -> {
      use _ <- result_try(set_status("gating", round))
      use regate_result <- result_try(run_gate(workspace, resumed.files_touched))
      case regate_result {
        GateResult(verdict: GatePass) ->
          case workflow.sleep(duration.milliseconds(input.round_backoff_ms)) {
            Ok(Nil) ->
              review_loop(
                input,
                workspace,
                resumed,
                regate_result,
                build_warm,
                verify_rounds,
                round + 1,
              )
            Error(engine_error) ->
              Error(StageFailed(
                stage: "review_backoff",
                message: errors.engine_message(engine_error),
              ))
          }
        GateResult(verdict: GateFail(report: report)) ->
          Error(GateRejected(report: report))
      }
    }
    Error(activity_error) ->
      Error(StageFailed(
        stage: "dev_resume",
        message: errors.activity_message(activity_error),
      ))
  }
}

/// Land only on Approve and a passing gate (both already established by the
/// caller).
fn land(
  workspace: Workspace,
  repo_root: String,
  base_ref: String,
  dev_result: DevResult,
  build_warm: BuildWarm,
  verify_rounds: Int,
  round: Int,
) -> Result(PipelineResult, PipelineError) {
  use _ <- result_try(set_status("landing", round))
  case
    workflow.run(
      activities.land(LandInput(
        workspace: workspace,
        repo_root: repo_root,
        base_ref: base_ref,
        dev_result: dev_result,
      )),
    )
  {
    Ok(landed) -> {
      use _ <- result_try(set_status("landed", round))
      Ok(PipelineResult(
        branch: landed.branch,
        merged_into: landed.merged_into,
        session_id: dev_result.session_id,
        build_warm: build_warm,
        verify_rounds: verify_rounds,
        review_rounds: round,
      ))
    }
    Error(activity_error) ->
      Error(LandFailed(message: errors.activity_message(activity_error)))
  }
}

/// Re-register the status handler with the current phase and round, so
/// `{{name}}_status` queries answer live state at every yield point
/// (re-registration per stage, per docs/guides/workflows.md).
fn set_status(phase: String, round: Int) -> Result(Nil, PipelineError) {
  let status = PipelineStatus(phase: phase, round: round)
  case
    query.handler(
      status_query_name,
      codecs_workflows.pipeline_status_codec(),
      fn() { status },
    )
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) ->
      Error(StageFailed(
        stage: "register_status",
        message: errors.query_message(query_error),
      ))
  }
}

fn child_engine_message(
  child_error: error.ChildError(child_workflow_error),
) -> String {
  case child_error {
    error.ChildWorkflowFailed(_) ->
      "child failed with an error the caller already handles"
    error.ChildOutputDecodeFailed(_) -> "child output could not be decoded"
    error.ChildErrorDecodeFailed(_) ->
      "child error payload could not be decoded"
    error.ChildEngineFailure(message: message) -> message
  }
}

fn result_try(
  result: Result(value, PipelineError),
  next: fn(value) -> Result(output, PipelineError),
) -> Result(output, PipelineError) {
  case result {
    Ok(value) -> next(value)
    Error(workflow_error) -> Error(workflow_error)
  }
}
