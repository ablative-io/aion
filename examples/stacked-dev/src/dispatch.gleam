//// The dispatch top-level workflow: a wave of briefed work in, a
//// machine-readable per-brief outcome report out (BD-006).
////
//// Control flow (S1, S14, S15, S16):
////
//// 1. `assemble_wave` (run exactly once): the dispatcher activity reads the
////    ledgers and cluster documents under `design_dir`, resolves every
////    reference, orders the wave by `depends_on`, and refuses a stale,
////    coverage-broken, or dependency-blocked wave (CN1). Refusal surfaces as
////    `AssemblyRefused`; the workflow body itself touches no file (CN1).
//// 2. FOR EACH assembled entry IN ORDER, strictly serially: re-register the
////    `dispatch_status` query, build a `StackedDevInput` from the shared
////    inputs plus the entry's resolved document/context, spawn the
////    `stacked_dev` child via `workflow.spawn_and_wait`, and record
////    `BriefLanded` on success or `BriefFailed` (the child's typed error
////    verbatim) on a typed child failure — a child's failure is recorded
////    data, never a dispatch failure (P3).
//// 3. IF a child fails AND `halt_on_failure`, record `BriefSkipped` for every
////    remaining entry and return; otherwise continue to the next entry.
////
//// Serial-only by design: bounded fan-out waits on parent-close
//// (RM-001/ADR-004); this module spawns no children concurrently, retries no
//// failed child in place, and a failed run is a terminal record whose retry is
//// a fresh dispatch (ADR-005).

import aion/codec
import aion/error
import aion/query
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/list
import stacked_dev
import stacked_dev/activities
import stacked_dev/codecs_dispatch
import stacked_dev/codecs_workflows
import stacked_dev/errors
import stacked_dev/types.{
  type AssembledWave, type BriefOutcome, type DispatchError, type DispatchInput,
  type DispatchResult, type DispatchStatus, type WaveEntry, AssembleInput,
  AssemblyRefused, BriefFailed, BriefLanded, BriefSkipped, DispatchResult,
  DispatchStageFailed, DispatchStatus, StackedDevInput,
}

/// The workflow type a caller dispatches and the parent passes to
/// `workflow.spawn_and_wait` — exactly this entry module's name.
pub const workflow_type = "dispatch"

/// Name of the live status query this workflow answers: current brief id,
/// 1-based position, wave total, and the per-brief outcomes so far.
pub const status_query_name = "dispatch_status"

/// The `stacked_dev` child type, resolved by entry-module name against the
/// engine's loaded packages.
const stacked_dev_type = "stacked_dev"

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  DispatchInput,
  DispatchResult,
  DispatchError,
) {
  workflow.define(
    "dispatch",
    codecs_dispatch.dispatch_input_codec(),
    codecs_dispatch.dispatch_result_codec(),
    codecs_dispatch.dispatch_error_codec(),
    execute,
  )
}

/// Engine entry point. The runtime delivers the start input as a raw JSON
/// string: decode it, run the typed workflow, and encode the success value
/// back to its JSON string for the recorded result payload.
pub fn run(raw_input: Dynamic) -> Result(String, DispatchError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs_dispatch.dispatch_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) ->
              Ok(codecs_dispatch.dispatch_result_codec().encode(output))
            Error(dispatch_error) -> Error(dispatch_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(DispatchStageFailed(
            stage: "decode_input",
            message: "failed to decode dispatch input: " <> reason,
          ))
      }
    Error(_) ->
      Error(DispatchStageFailed(
        stage: "decode_input",
        message: "dispatch input payload was not a string",
      ))
  }
}

/// Typed workflow body: register the status query, assemble the wave once,
/// then run one `stacked_dev` child per entry strictly serially, recording an
/// outcome each and re-registering the status query at every transition.
pub fn execute(input: DispatchInput) -> Result(DispatchResult, DispatchError) {
  use _ <- result_try(
    set_status(
      DispatchStatus(current_brief: "", position: 0, total: 0, outcomes: []),
    ),
  )
  use wave <- result_try(assemble(input))
  let total = list.length(wave.entries)
  use outcomes <- result_try(
    dispatch_entries(input, wave.entries, total, 1, []),
  )
  use _ <- result_try(
    set_status(DispatchStatus(
      current_brief: "",
      position: total,
      total: total,
      outcomes: outcomes,
    )),
  )
  Ok(DispatchResult(outcomes: outcomes))
}

/// Run `assemble_wave` exactly once. A refusal or can't-execute condition the
/// activity raised becomes `AssemblyRefused` (the only assembly-side dispatch
/// error); the workflow never reads a file itself (CN1).
fn assemble(input: DispatchInput) -> Result(AssembledWave, DispatchError) {
  case
    workflow.run(
      activities.assemble_wave(AssembleInput(
        design_dir: input.design_dir,
        wave: input.wave,
      )),
    )
  {
    Ok(wave) -> Ok(wave)
    Error(activity_error) ->
      Error(AssemblyRefused(message: errors.activity_message(activity_error)))
  }
}

/// Process the entries serially, threading the reversed outcome accumulator.
/// Re-registers the status query before each child (current brief, position,
/// outcomes so far). On a `BriefFailed` with `halt_on_failure`, marks every
/// remaining entry `BriefSkipped` and returns without spawning further.
fn dispatch_entries(
  input: DispatchInput,
  entries: List(WaveEntry),
  total: Int,
  position: Int,
  outcomes_reversed: List(BriefOutcome),
) -> Result(List(BriefOutcome), DispatchError) {
  case entries {
    [] -> Ok(list.reverse(outcomes_reversed))
    [entry, ..rest] -> {
      use _ <- result_try(
        set_status(DispatchStatus(
          current_brief: entry.brief_document.id,
          position: position,
          total: total,
          outcomes: list.reverse(outcomes_reversed),
        )),
      )
      use outcome <- result_try(run_child(input, entry))
      let outcomes_reversed = [outcome, ..outcomes_reversed]
      case is_failure(outcome) && input.halt_on_failure {
        True -> {
          let skipped =
            list.map(rest, fn(remaining) {
              BriefSkipped(
                brief_id: remaining.brief_document.id,
                after: entry.brief_document.id,
              )
            })
          Ok(list.append(list.reverse(outcomes_reversed), skipped))
        }
        False ->
          dispatch_entries(input, rest, total, position + 1, outcomes_reversed)
      }
    }
  }
}

/// Spawn the `stacked_dev` child for one entry and lift its result into a
/// recorded outcome. A typed child failure is `BriefFailed` (recorded data,
/// P3); only engine-level child errors become a `DispatchStageFailed`.
fn run_child(
  input: DispatchInput,
  entry: WaveEntry,
) -> Result(BriefOutcome, DispatchError) {
  let brief_id = entry.brief_document.id
  case
    workflow.spawn_and_wait(
      stacked_dev_type,
      stacked_dev.execute,
      StackedDevInput(
        repo_root: input.repo_root,
        brief_id: brief_id,
        reviewers: input.reviewers,
        base_ref: input.base_ref,
        placement: input.placement,
        isolation: input.isolation,
        brief_document: entry.brief_document,
        resolved_context: entry.resolved_context,
        verify_fix_cap: input.verify_fix_cap,
        review_cap: input.review_cap,
        round_backoff_ms: input.round_backoff_ms,
        review_deadline_ms: input.review_deadline_ms,
      ),
      codecs_workflows.stacked_dev_input_codec(),
      codecs_workflows.stacked_dev_result_codec(),
      codecs_workflows.stacked_dev_error_codec(),
    )
  {
    Ok(result) ->
      Ok(BriefLanded(
        brief_id: brief_id,
        branch: result.branch,
        merged_into: result.merged_into,
      ))
    Error(error.ChildWorkflowFailed(child_error)) ->
      Ok(BriefFailed(brief_id: brief_id, error: child_error))
    Error(other) ->
      Error(DispatchStageFailed(
        stage: "spawn_stacked_dev",
        message: child_engine_message(other),
      ))
  }
}

fn is_failure(outcome: BriefOutcome) -> Bool {
  case outcome {
    BriefFailed(brief_id: _, error: _) -> True
    _ -> False
  }
}

/// Re-register the status handler so `dispatch_status` queries answer live
/// state at every transition (re-registration per transition re-arms it on
/// replay automatically).
fn set_status(status: DispatchStatus) -> Result(Nil, DispatchError) {
  case
    query.handler(
      status_query_name,
      codecs_dispatch.dispatch_status_codec(),
      fn() { status },
    )
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) ->
      Error(DispatchStageFailed(
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
  result: Result(value, DispatchError),
  next: fn(value) -> Result(output, DispatchError),
) -> Result(output, DispatchError) {
  case result {
    Ok(value) -> next(value)
    Error(dispatch_error) -> Error(dispatch_error)
  }
}
