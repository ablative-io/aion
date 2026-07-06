//// The implement_and_gate workflow: brief in → reviewed-ready diff in an
//// isolated workspace, with the gate battery green as RECORDED FACT
//// (prospekt doctrine: workflows/implement-and-gate.md).
////
//// The heart of "agents propose, activities verify": the implementer is an
//// agent; every gate is a command activity whose own exit status lands in
//// durable history. The workflow cannot reach `GatesGreen` with a red gate
//// — that is topology, not policy.
////
//// Topology:
////
//// 1. `provision_workspace` — an isolated worktree/clone of `repo_root` at
////    `base_ref`; failure is terminal (nothing downstream can run).
//// 2. `implement` — norn run INSIDE the workspace (the stacked-dev dev
////    pattern), prompt = implementer profile + the brief verbatim +
////    `out_of_scope` called out
////    (`schemas/implementation-report.schema.json`).
//// 3. The gate battery, in declared order: each `run_gate` shells the exact
////    command in the workspace and records `CliRun{exit_status, output,
////    duration_ms}` — a non-zero exit is DATA, never an activity error; the
////    battery short-circuits at the first red gate.
//// 4. On a red gate: `fix_round < fix_cap` → `implement_resume` hands the
////    SAME session the failing gate's captured output (the durable record,
////    tail-bounded at capture — never a paraphrase), then ALL gates re-run
////    from the top; `fix_round == fix_cap` → the run completes
////    `GatesExhausted` carrying the full gate record — a finding surfaced
////    to the operator, never an error crash.
//// 5. All gates exit 0 → `GatesGreen` with the last report, the green
////    record, rounds, and the workspace path.
////
//// The workspace is intentionally NOT torn down on either terminus: review
//// needs the diff on success, the operator inspects on exhaustion. The
//// `teardown_workspace` activity exists as a declared seam for future
//// wiring but is deliberately never dispatched here.

import aion/codec
import aion/workflow
import dev_pipeline/activities
import dev_pipeline/codecs
import dev_pipeline/errors
import dev_pipeline/prompts
import dev_pipeline/types.{
  type GateCliRun, type GateRecordEntry, type GateSpec,
  type ImplementAndGateError, type ImplementAndGateInput,
  type ImplementAndGateResult, type ImplementationReport, type Workspace,
  GateRecordEntry, GateRun, GatesExhausted, GatesGreen, ImplementAndGateResult,
  ImplementAndGateStageFailed, ImplementRound, ProvisionInput,
}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/list
import gleam/option.{None, Some}

/// The deployed workflow type: the entry module name.
pub const workflow_type = "implement_and_gate"

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  ImplementAndGateInput,
  ImplementAndGateResult,
  ImplementAndGateError,
) {
  workflow.define(
    "implement-and-gate",
    codecs.implement_and_gate_input_codec(),
    codecs.implement_and_gate_result_codec(),
    codecs.implement_and_gate_error_codec(),
    execute,
  )
}

/// Engine entry point for one execution. The runtime delivers the start
/// input as a raw JSON string; success and failure are both encoded back to
/// JSON text here — the engine records these exact payloads as the workflow
/// terminal.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.implement_and_gate_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) ->
              Ok(codecs.implement_and_gate_result_codec().encode(output))
            Error(stage_error) ->
              Error(codecs.implement_and_gate_error_codec().encode(stage_error))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            codecs.implement_and_gate_error_codec().encode(
              ImplementAndGateStageFailed(
                stage: "decode_input",
                message: "failed to decode implement-and-gate input: " <> reason,
              ),
            ),
          )
      }
    Error(_) ->
      Error(
        codecs.implement_and_gate_error_codec().encode(
          ImplementAndGateStageFailed(
            stage: "decode_input",
            message: "implement-and-gate input payload was not a string",
          ),
        ),
      )
  }
}

/// Typed workflow body: provision, the initial implementer round, then the
/// capped gate⇄fix loop.
pub fn execute(
  input: ImplementAndGateInput,
) -> Result(ImplementAndGateResult, ImplementAndGateError) {
  case provision(input) {
    Ok(workspace) ->
      case run_implement(input, workspace) {
        Ok(report) -> gate_loop(input, workspace, report, 0)
        Error(stage_error) -> Error(stage_error)
      }
    Error(stage_error) -> Error(stage_error)
  }
}

/// Provision the isolated workspace. Terminal on failure — there is nothing
/// to fix-loop without a workspace.
fn provision(
  input: ImplementAndGateInput,
) -> Result(Workspace, ImplementAndGateError) {
  case
    workflow.run(activities.provision_workspace(
      ProvisionInput(
        repo_root: input.repo_root,
        base_ref: input.base_ref,
        isolation: input.isolation,
        task_ref: input.brief.task_ref,
      ),
      input.node,
    ))
  {
    Ok(workspace) -> Ok(workspace)
    Error(activity_error) ->
      Error(ImplementAndGateStageFailed(
        stage: "provision_workspace",
        message: errors.activity_message(activity_error),
      ))
  }
}

/// The initial implementer round in its deterministic
/// `<task_ref>-implement` session: the brief embedded verbatim,
/// `out_of_scope` called out.
fn run_implement(
  input: ImplementAndGateInput,
  workspace: Workspace,
) -> Result(ImplementationReport, ImplementAndGateError) {
  let brief_json = codecs.brief_codec().encode(input.brief)
  case
    workflow.run(activities.implementer(
      ImplementRound(
        workspace_path: workspace.path,
        session_id: input.brief.task_ref <> "-implement",
        prompt: prompts.implement_prompt(input, brief_json, workspace.path),
        model: input.implementer_model,
      ),
      input.node,
    ))
  {
    Ok(report) -> Ok(report)
    Error(activity_error) ->
      Error(ImplementAndGateStageFailed(
        stage: "implement",
        message: errors.activity_message(activity_error),
      ))
  }
}

/// How one battery run through the declared gates ended: every gate green,
/// or short-circuited at the first red gate (the record carries the red
/// entry last, with its tail-bounded output).
type BatteryVerdict {
  AllGreen(record: List(GateRecordEntry))
  FirstRed(record: List(GateRecordEntry), gate: GateSpec, cli_run: GateCliRun)
}

/// One gate⇄fix round. `fix_round` counts the resumes already spent: the
/// initial battery runs at 0; a red battery at `fix_round < fix_cap` buys an
/// `implement_resume` and a full re-run; at `fix_round == fix_cap` the run
/// completes `GatesExhausted` — surfaced, never crashed.
fn gate_loop(
  input: ImplementAndGateInput,
  workspace: Workspace,
  report: ImplementationReport,
  fix_round: Int,
) -> Result(ImplementAndGateResult, ImplementAndGateError) {
  case run_battery(input, workspace, input.gates, []) {
    Ok(AllGreen(record)) ->
      Ok(ImplementAndGateResult(
        outcome: GatesGreen,
        implementation_report: report,
        gate_record: record,
        rounds: fix_round + 1,
        workspace_path: workspace.path,
      ))
    Ok(FirstRed(record, gate, cli_run)) ->
      case fix_round >= input.fix_cap {
        True ->
          Ok(ImplementAndGateResult(
            outcome: GatesExhausted,
            implementation_report: report,
            gate_record: record,
            rounds: fix_round + 1,
            workspace_path: workspace.path,
          ))
        False ->
          case run_resume(input, workspace, gate, cli_run, fix_round + 1) {
            Ok(replacement_report) ->
              gate_loop(input, workspace, replacement_report, fix_round + 1)
            Error(stage_error) -> Error(stage_error)
          }
      }
    Error(stage_error) -> Error(stage_error)
  }
}

/// Run the declared gates in order, short-circuiting at the first non-zero
/// exit. Only a gate whose binary/workspace is missing errors here (the
/// activity's terminal failure); a red exit is recorded data.
fn run_battery(
  input: ImplementAndGateInput,
  workspace: Workspace,
  gates: List(GateSpec),
  seen: List(GateRecordEntry),
) -> Result(BatteryVerdict, ImplementAndGateError) {
  case gates {
    [] -> Ok(AllGreen(list.reverse(seen)))
    [gate, ..rest] ->
      case
        workflow.run(activities.run_gate(
          GateRun(
            workspace_path: workspace.path,
            gate_id: gate.id,
            command: gate.command,
          ),
          input.node,
        ))
      {
        Ok(cli_run) ->
          case cli_run.exit_status {
            0 ->
              run_battery(input, workspace, rest, [
                record_entry(gate, cli_run, None),
                ..seen
              ])
            _ ->
              Ok(FirstRed(
                list.reverse([
                  record_entry(gate, cli_run, Some(cli_run.output)),
                  ..seen
                ]),
                gate,
                cli_run,
              ))
          }
        Error(activity_error) ->
          Error(ImplementAndGateStageFailed(
            stage: "gate " <> gate.id,
            message: errors.activity_message(activity_error),
          ))
      }
  }
}

/// One fix round: resume the SAME implementer session with the failing
/// gate's captured output. Returns a FULL replacement report.
fn run_resume(
  input: ImplementAndGateInput,
  workspace: Workspace,
  gate: GateSpec,
  cli_run: GateCliRun,
  fix_round: Int,
) -> Result(ImplementationReport, ImplementAndGateError) {
  case
    workflow.run(activities.implement_resume(
      ImplementRound(
        workspace_path: workspace.path,
        session_id: input.brief.task_ref <> "-implement",
        prompt: prompts.implement_resume_prompt(
          input.brief.task_ref,
          gate,
          cli_run,
          fix_round,
        ),
        model: input.implementer_model,
      ),
      input.node,
    ))
  {
    Ok(report) -> Ok(report)
    Error(activity_error) ->
      Error(ImplementAndGateStageFailed(
        stage: "implement_resume round " <> int.to_string(fix_round),
        message: errors.activity_message(activity_error),
      ))
  }
}

/// Shape one battery row: the gate, its exact command, the command's own
/// exit status and duration; `output_tail` rides only on the failing entry.
fn record_entry(
  gate: GateSpec,
  cli_run: GateCliRun,
  output_tail: option.Option(String),
) -> GateRecordEntry {
  GateRecordEntry(
    id: gate.id,
    command: gate.command,
    exit_status: cli_run.exit_status,
    duration_ms: cli_run.duration_ms,
    output_tail: output_tail,
  )
}
