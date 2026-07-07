//// The dev-brief pipeline: one development brief, end-to-end, on any stack
//// repo — the remediation flow's PROVEN skeleton (driven norn agents,
//// mechanical shell gates, derive-and-check adversarial verdicts, bounded
//// fix cycles, honest terminal dispositions) generalized from ledger
//// findings to arbitrary briefs.
////
//// The body:
////   provision an isolated worktree (branch `dev/<brief-id>` on the
////   configured base)
////   -> the bounded fix cycle (`dev_brief/cycle`) as a trampoline:
////        developer (AGENT, driven; session `{workflow_id}-developer`
////        resumed across cycles; the WORKER commits the round's work —
////        agents run no git)
////        -> run_gates (SHELL, fully mechanical: the brief's CONFIGURED
////           commands, exit-status-is-data; plus the diff since the base
////           commit) — a red gate loops back to the developer
////        -> the REVIEW FAN-OUT: one `review_lens` CHILD WORKFLOW per lens,
////           spawned CONCURRENTLY (each lens = its own workflow id = its own
////           norn session, visibly running in parallel in the ops console),
////           awaited, then judged with DERIVE-AND-CHECK: the loop decision
////           flows through the overall DERIVED from each verdict's findings;
////           an asserted overall that disagrees (or a rejection without a
////           reject_reason) is itself a rejection, recorded on the result —
////           the machine loops back with every verdict attached
////      cycle-capped; exhaustion is a TERMINAL DISPOSITION recorded on the
////      result, never a silent success
////   -> cleanup (SHELL: remove the worktree; the branch and its commits
////      remain; a dirty worktree is left in place).
////
//// Nothing here pushes anywhere: the handoff is the branch plus the result's
//// evidence (report, gate runs, every verdict) — the operator merges.

import aion/child
import aion/codec
import aion/error
import aion/workflow
import dev_brief/activities
import dev_brief/codecs
import dev_brief/cycle
import dev_brief/types.{
  type Brief, type BriefInput, type BriefResult, type DevBriefError,
  type DevReport, type Disposition, type GateOutcome, type Lens,
  type LensVerdict, type WorkspaceInfo, Accepted, BriefResult, ChildFailed,
  CleanupInput, CycleCapExhausted, DeveloperInput, GateInput, LensInput,
  ProvisionInput, StageFailed,
}
import dev_brief/verdicts
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/string
import review_lens

/// The workflow and the worker agree on this base directory for brief
/// workspaces: each brief's worktree lives at `<base>/<workflow_id>`. The
/// Rust worker's developer harness points norn's `--workspace-root` at the
/// SAME `<base>/{workflow_id}` template, so the driven agent operates in
/// exactly the worktree the `provision_workspace` activity created. Keep
/// this in sync with `WORKSPACE_BASE` in `worker/src/handlers.rs`.
pub const workspace_base = "/tmp/aion-dev/ws"

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  BriefInput,
  BriefResult,
  DevBriefError,
) {
  workflow.define(
    "dev_brief",
    codecs.brief_input_codec(),
    codecs.brief_result_codec(),
    codecs.dev_brief_error_codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, DevBriefError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.brief_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(result) -> Ok(codecs.brief_result_codec().encode(result))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(types.DecodeInputFailed(
            "failed to decode brief input: " <> reason,
          ))
      }
    Error(_) ->
      Error(types.DecodeInputFailed("brief input payload was not a string"))
  }
}

/// The pipeline body: provision, then the bounded fix cycle, then the
/// mechanical cleanup tail.
pub fn execute(input: BriefInput) -> Result(BriefResult, DevBriefError) {
  let cap =
    cycle.resolve_cap(
      input.config.max_fix_cycles,
      types.default_max_fix_cycles(),
    )
  let lenses = resolve_lenses(input.config.lenses)

  use workspace <- try(provision(input))
  let state =
    LoopState(
      workspace: workspace,
      report: None,
      last_gate: None,
      verdicts: [],
      verdict_mismatches: [],
    )
  drive(input, lenses, cycle.initial(cap), state)
}

/// The lens set a run reviews with: the configured lenses, or the default
/// adversarial trio when the input names none. Zero lenses can never review
/// a diff into acceptance (`verdicts.all_accept` rejects the empty round),
/// so an empty configuration resolves to the defaults rather than looping a
/// brief to exhaustion by construction.
pub fn resolve_lenses(configured: List(Lens)) -> List(Lens) {
  case configured {
    [] -> types.default_lenses()
    lenses -> lenses
  }
}

/// The carried artifacts alongside the pure cap machine: the workspace and
/// the most recent developer/gate/review results, used to compose the next
/// activity input and to build the terminal [`BriefResult`].
/// `verdict_mismatches` accumulates every derive-and-check violation and
/// every lost lens (cycle-stamped) — evidence for the operator, surfaced on
/// the result.
type LoopState {
  LoopState(
    workspace: WorkspaceInfo,
    report: Option(DevReport),
    last_gate: Option(GateOutcome),
    verdicts: List(LensVerdict),
    verdict_mismatches: List(String),
  )
}

/// The trampoline: ask the machine for the next instruction, perform exactly
/// that one effect, fold the outcome back, recurse.
fn drive(
  input: BriefInput,
  lenses: List(Lens),
  machine: cycle.Machine,
  state: LoopState,
) -> Result(BriefResult, DevBriefError) {
  case cycle.plan(machine) {
    cycle.Stop(disposition) ->
      finalize(
        input,
        state,
        disposition: disposition,
        fix_cycles: machine.fix_rounds,
        detail: stop_detail(disposition, state),
      )
    cycle.Developer -> {
      use report <- try(run_developer(input, state))
      drive(
        input,
        lenses,
        cycle.on_developer(machine),
        LoopState(..state, report: Some(report)),
      )
    }
    cycle.Gate -> {
      use outcome <- try(run_gates(input, state))
      drive(
        input,
        lenses,
        cycle.on_gate(machine, outcome.pass),
        LoopState(..state, last_gate: Some(outcome)),
      )
    }
    cycle.Review -> {
      use collected <- try(fan_out_review(input, lenses, state))
      // DERIVE-AND-CHECK over every lens verdict: the loop decision flows
      // through each verdict's DERIVED overall, never the asserted one; any
      // asserted-vs-derived disagreement, missing reject_reason, or lens
      // that returned no verdict is recorded, cycle-stamped, for the
      // operator — and treated as a rejection, never a silent acceptance.
      let issues =
        list.append(
          list.flat_map(collected, verdicts.verdict_issues),
          list.map(verdicts.missing_lenses(lenses, collected), fn(name) {
            "lens " <> name <> " returned no verdict"
          }),
        )
      let stamped =
        list.map(issues, fn(issue) {
          "cycle " <> int.to_string(machine.fix_rounds) <> ": " <> issue
        })
      let accepted =
        verdicts.all_accept(collected)
        && verdicts.missing_lenses(lenses, collected) == []
      drive(
        input,
        lenses,
        cycle.on_review(machine, accepted),
        LoopState(
          ..state,
          verdicts: collected,
          verdict_mismatches: list.append(state.verdict_mismatches, stamped),
        ),
      )
    }
  }
}

// --- effects ------------------------------------------------------------------

fn provision(input: BriefInput) -> Result(WorkspaceInfo, DevBriefError) {
  use workflow_id <- try(engine_id())
  let workspace_path = workspace_base <> "/" <> workflow_id
  let branch = "dev/" <> verdicts.branch_safe(input.brief.id)
  case
    workflow.run(
      activities.provision(ProvisionInput(
        repo_root: input.config.repo_root,
        base_branch: input.config.base_branch,
        branch: branch,
        workspace_path: workspace_path,
      )),
    )
  {
    Ok(info) -> Ok(info)
    Error(activity_error) -> stage_error("provision_workspace", activity_error)
  }
}

fn run_developer(
  input: BriefInput,
  state: LoopState,
) -> Result(DevReport, DevBriefError) {
  // The workspace path rides along so the WORKER can commit the round's work
  // after a successful turn (agents do not run git) — and the returned
  // report's `commits` carries the REAL branch head the worker made, not an
  // agent-asserted hash.
  case
    workflow.run(
      activities.developer(DeveloperInput(
        brief: input.brief,
        gate: state.last_gate,
        verdicts: state.verdicts,
        workspace_path: state.workspace.workspace_path,
        gates: input.config.gates,
      )),
    )
  {
    Ok(report) -> Ok(report)
    Error(activity_error) -> stage_error("developer", activity_error)
  }
}

fn run_gates(
  input: BriefInput,
  state: LoopState,
) -> Result(GateOutcome, DevBriefError) {
  case
    workflow.run(
      activities.run_gates(GateInput(
        workspace_path: state.workspace.workspace_path,
        base_commit: state.workspace.base_commit,
        gates: input.config.gates,
      )),
    )
  {
    Ok(outcome) -> Ok(outcome)
    Error(activity_error) -> stage_error("run_gates", activity_error)
  }
}

/// THE INTRA-BRIEF FAN-OUT: spawn one `review_lens` child per lens,
/// CONCURRENTLY; the awaits then collect their verdicts in order. While this
/// runs, every lens is a live agent session of its own — parallel work
/// inside one brief, visible as sibling workflows in the ops console.
fn fan_out_review(
  input: BriefInput,
  lenses: List(Lens),
  state: LoopState,
) -> Result(List(LensVerdict), DevBriefError) {
  // Both artifacts exist whenever the machine reaches the review: the
  // developer and the gate precede it on every path. Their absence is an
  // engine-ordering fault surfaced loudly, never defaulted around.
  case state.report, state.last_gate {
    Some(report), Some(gate) -> {
      use handles <- try(spawn_lenses(input.brief, lenses, report, gate))
      await_all(handles, [])
    }
    _, _ ->
      Error(StageFailed(
        stage: "review",
        message: "review reached without a dev report and a gate outcome — "
          <> "cycle-machine ordering violated",
      ))
  }
}

fn spawn_lenses(
  brief: Brief,
  lenses: List(Lens),
  report: DevReport,
  gate: GateOutcome,
) -> Result(List(child.ChildHandle(LensVerdict, DevBriefError)), DevBriefError) {
  case lenses {
    [] -> Ok([])
    [lens, ..rest] ->
      case
        child.spawn(
          "review_lens",
          review_lens.execute,
          LensInput(
            lens: lens,
            brief: brief,
            diff: gate.diff,
            report: report,
            gate_runs: gate.runs,
          ),
          codecs.lens_input_codec(),
          codecs.lens_verdict_codec(),
          codecs.dev_brief_error_codec(),
        )
      {
        Ok(handle) -> {
          use rest_handles <- try(spawn_lenses(brief, rest, report, gate))
          Ok([handle, ..rest_handles])
        }
        Error(engine_error) ->
          Error(ChildFailed(
            reason: "could not spawn lens "
            <> lens.name
            <> ": "
            <> string.inspect(engine_error),
          ))
      }
  }
}

fn await_all(
  handles: List(child.ChildHandle(LensVerdict, DevBriefError)),
  acc: List(LensVerdict),
) -> Result(List(LensVerdict), DevBriefError) {
  case handles {
    [] -> Ok(list.reverse(acc))
    [handle, ..rest] ->
      case child.await(handle) {
        Ok(verdict) -> await_all(rest, [verdict, ..acc])
        Error(child_error) -> Error(child_error_to_dev_brief(child_error))
      }
  }
}

fn child_error_to_dev_brief(
  child_error: error.ChildError(DevBriefError),
) -> DevBriefError {
  case child_error {
    // A lens child that failed with a typed dev-brief error propagates it —
    // the parent's history carries the child's own taxonomy.
    error.ChildWorkflowFailed(dev_brief_error) -> dev_brief_error
    error.ChildOutputDecodeFailed(_) ->
      ChildFailed(reason: "lens verdict could not be decoded")
    error.ChildErrorDecodeFailed(_) ->
      ChildFailed(reason: "lens child error could not be decoded")
    error.ChildEngineFailure(message: message) ->
      ChildFailed(reason: "lens child engine failure: " <> message)
  }
}

// --- the mechanical tail: cleanup + result ------------------------------------

/// Remove the worktree and build the terminal result. Cleanup refusals are
/// RECORDED on the result (`workspace_removed: False`), never swallowed.
fn finalize(
  input: BriefInput,
  state: LoopState,
  disposition disposition: Disposition,
  fix_cycles fix_cycles: Int,
  detail detail: String,
) -> Result(BriefResult, DevBriefError) {
  use cleanup <- try(run_cleanup(input, state.workspace))
  let first_pass_accepted = disposition == Accepted && fix_cycles == 1
  Ok(BriefResult(
    brief_id: input.brief.id,
    disposition: disposition,
    fix_cycles: fix_cycles,
    first_pass_accepted: first_pass_accepted,
    verdict_mismatches: state.verdict_mismatches,
    branch: state.workspace.branch,
    report: state.report,
    gate: state.last_gate,
    verdicts: state.verdicts,
    workspace_removed: cleanup.removed,
    summary: brief_summary(input, disposition, fix_cycles, detail),
  ))
}

fn run_cleanup(
  input: BriefInput,
  workspace: WorkspaceInfo,
) -> Result(types.CleanupOutcome, DevBriefError) {
  case
    workflow.run(
      activities.cleanup(CleanupInput(
        repo_root: input.config.repo_root,
        workspace_path: workspace.workspace_path,
      )),
    )
  {
    Ok(outcome) -> Ok(outcome)
    Error(activity_error) -> stage_error("cleanup_workspace", activity_error)
  }
}

// --- summaries -------------------------------------------------------------------

fn stop_detail(disposition: Disposition, state: LoopState) -> String {
  case disposition {
    Accepted -> "every lens accepted"
    CycleCapExhausted ->
      "fix-cycle budget exhausted; last adverse evidence: "
      <> last_adverse_evidence(state)
  }
}

fn last_adverse_evidence(state: LoopState) -> String {
  case verdicts.adverse_lines(state.verdicts) {
    [_, ..] as lines -> string.join(lines, "; ")
    [] ->
      case state.last_gate {
        Some(gate) if !gate.pass -> "gate red: " <> gate.diagnostics
        _ -> "no gate or verdict evidence recorded"
      }
  }
}

fn brief_summary(
  input: BriefInput,
  disposition: Disposition,
  fix_cycles: Int,
  detail: String,
) -> String {
  "Brief "
  <> input.brief.id
  <> ": "
  <> types.disposition_to_string(disposition)
  <> " after "
  <> int.to_string(fix_cycles)
  <> " fix cycle(s) on branch "
  <> "dev/"
  <> verdicts.branch_safe(input.brief.id)
  <> ". "
  <> detail
}

// --- helpers ---------------------------------------------------------------------

/// The workflow's own id — the scope the workspace path and the developer's
/// norn session id are keyed on.
fn engine_id() -> Result(String, DevBriefError) {
  case workflow.id() {
    Ok(id) -> Ok(id)
    Error(engine_error) ->
      Error(StageFailed(
        stage: "workflow_id",
        message: "could not read the workflow id: "
          <> string.inspect(engine_error),
      ))
  }
}

fn stage_error(
  stage: String,
  activity_error: error.ActivityError,
) -> Result(value, DevBriefError) {
  Error(StageFailed(stage: stage, message: activity_message(activity_error)))
}

fn activity_message(activity_error: error.ActivityError) -> String {
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

/// `use`-friendly bind over `Result` with [`DevBriefError`].
fn try(
  result: Result(a, DevBriefError),
  next: fn(a) -> Result(b, DevBriefError),
) -> Result(b, DevBriefError) {
  case result {
    Ok(value) -> next(value)
    Error(dev_brief_error) -> Error(dev_brief_error)
  }
}
