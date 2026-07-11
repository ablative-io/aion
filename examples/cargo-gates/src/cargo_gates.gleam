//// cargo_gates: every gate, one run — check, clippy, tests, fmt fan out together
//// against one workspace and come back as data. Real exit codes, never pass-claims:
//// the verdict is computed from what actually ran, and a red gate names itself.

import aion/activity
import aion/awl/codec as awlc
import aion/awl/error as awl_error
import aion/awl/runtime
import aion/codec.{type Codec}
import aion/duration
import aion/error
import aion/signal
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/option.{type Option, None, Some}
import gleam/result

/// One gate's actual result — exit status is data, never a silent failure.
pub type GateResult {
  GateResult(
    gate: String,
    exit_code: Int,
    passed: Bool,
    output_tail: String,
  )
}

pub type Clean {
  Clean(
    gates_run: Int,
  )
}

/// Carries ALL gate results, passed flags included — the VM cannot build a
/// failed-only sublist from named-fork bindings (workbench finding, F16 family).
pub type Failing {
  Failing(
    gates: List(GateResult),
  )
}

pub type CargoGatesInput {
  CargoGatesInput(
    workspace_path: String,
  )
}

pub type CargoGatesOutcome {
  CleanOutcome(Clean)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(CargoGatesInput, CargoGatesOutcome, awl_error.AwlError) {
  workflow.define(
    "cargo_gates",
    cargo_gates_input_codec(),
    cargo_gates_outcome_codec(),
    awl_error.codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, awl_error.AwlError) {
  runtime.run(raw_input, cargo_gates_input_codec(), cargo_gates_outcome_codec(), execute)
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: CargoGatesInput) -> Result(CargoGatesOutcome, awl_error.AwlError) {
  let workspace_path = input.workspace_path
  step_run_gates(workspace_path)
}

fn step_run_gates(workspace_path: String) -> Result(CargoGatesOutcome, awl_error.AwlError) {
  use awl_branches <- result.try(workflow.all([run_check_activity_raw(workspace_path) |> activity.timeout(duration.milliseconds(900000)) |> activity.task_queue("cargo_gates") |> activity.node("shell"), run_clippy_activity_raw(workspace_path) |> activity.timeout(duration.milliseconds(900000)) |> activity.task_queue("cargo_gates") |> activity.node("shell"), run_tests_activity_raw(workspace_path) |> activity.timeout(duration.milliseconds(2700000)) |> activity.task_queue("cargo_gates") |> activity.node("shell"), run_fmt_check_activity_raw(workspace_path) |> activity.timeout(duration.milliseconds(300000)) |> activity.task_queue("cargo_gates") |> activity.node("shell")]) |> awl_error.map_activity_error)
  let assert [awl_raw_0, awl_raw_1, awl_raw_2, awl_raw_3] = awl_branches
  use check_gate <- result.try(awlc.decoded(gate_result_codec(), awl_raw_0, "run_check"))
  use clippy_gate <- result.try(awlc.decoded(gate_result_codec(), awl_raw_1, "run_clippy"))
  use test_gate <- result.try(awlc.decoded(gate_result_codec(), awl_raw_2, "run_tests"))
  use fmt_gate <- result.try(awlc.decoded(gate_result_codec(), awl_raw_3, "run_fmt_check"))
  case { { check_gate.passed && clippy_gate.passed } && test_gate.passed } && fmt_gate.passed {
    True -> {
      Ok(CleanOutcome(Clean(gates_run: 4)))
    }
    False -> {
      Error(awl_error.AwlOutcomeFailure("failing", json.to_string(failing_to_json(Failing(gates: [check_gate, clippy_gate, test_gate, fmt_gate])))))
    }
  }
}

pub type RunCheckInput {
  RunCheckInput(
    path: String,
  )
}

fn run_check_activity(
  path: String,
) -> activity.Activity(RunCheckInput, GateResult) {
  activity.new(
    "run_check",
    RunCheckInput(
      path: path,
    ),
    run_check_input_codec(),
    gate_result_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn run_check_activity_raw(
  path: String,
) -> activity.Activity(String, String) {
  let awl_input_codec = run_check_input_codec()
  activity.new(
    "run_check",
    awl_input_codec.encode(RunCheckInput(
      path: path,
    )),
    awlc.raw(),
    awlc.raw(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type RunClippyInput {
  RunClippyInput(
    path: String,
  )
}

fn run_clippy_activity(
  path: String,
) -> activity.Activity(RunClippyInput, GateResult) {
  activity.new(
    "run_clippy",
    RunClippyInput(
      path: path,
    ),
    run_clippy_input_codec(),
    gate_result_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn run_clippy_activity_raw(
  path: String,
) -> activity.Activity(String, String) {
  let awl_input_codec = run_clippy_input_codec()
  activity.new(
    "run_clippy",
    awl_input_codec.encode(RunClippyInput(
      path: path,
    )),
    awlc.raw(),
    awlc.raw(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type RunTestsInput {
  RunTestsInput(
    path: String,
  )
}

fn run_tests_activity(
  path: String,
) -> activity.Activity(RunTestsInput, GateResult) {
  activity.new(
    "run_tests",
    RunTestsInput(
      path: path,
    ),
    run_tests_input_codec(),
    gate_result_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn run_tests_activity_raw(
  path: String,
) -> activity.Activity(String, String) {
  let awl_input_codec = run_tests_input_codec()
  activity.new(
    "run_tests",
    awl_input_codec.encode(RunTestsInput(
      path: path,
    )),
    awlc.raw(),
    awlc.raw(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type RunFmtCheckInput {
  RunFmtCheckInput(
    path: String,
  )
}

fn run_fmt_check_activity(
  path: String,
) -> activity.Activity(RunFmtCheckInput, GateResult) {
  activity.new(
    "run_fmt_check",
    RunFmtCheckInput(
      path: path,
    ),
    run_fmt_check_input_codec(),
    gate_result_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn run_fmt_check_activity_raw(
  path: String,
) -> activity.Activity(String, String) {
  let awl_input_codec = run_fmt_check_input_codec()
  activity.new(
    "run_fmt_check",
    awl_input_codec.encode(RunFmtCheckInput(
      path: path,
    )),
    awlc.raw(),
    awlc.raw(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn cargo_gates_input_codec() -> Codec(CargoGatesInput) {
  codec.json_codec(cargo_gates_input_to_json, cargo_gates_input_decoder())
}

fn cargo_gates_input_to_json(value: CargoGatesInput) -> json.Json {
  json.object([
    #("workspace_path", awlc.string_to_json(value.workspace_path)),
  ])
}

fn cargo_gates_input_decoder() -> decode.Decoder(CargoGatesInput) {
  use workspace_path <- decode.field("workspace_path", awlc.string_decoder())
  decode.success(CargoGatesInput(
    workspace_path: workspace_path,
  ))
}

fn cargo_gates_outcome_codec() -> Codec(CargoGatesOutcome) {
  codec.json_codec(cargo_gates_outcome_to_json, cargo_gates_outcome_decoder())
}

fn cargo_gates_outcome_to_json(value: CargoGatesOutcome) -> json.Json {
  case value {
    CleanOutcome(payload) -> json.object([#("outcome", json.string("clean")), #("payload", clean_to_json(payload))])
  }
}

fn cargo_gates_outcome_decoder() -> decode.Decoder(CargoGatesOutcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "clean" -> {
      use payload <- decode.field("payload", clean_decoder())
      decode.success(CleanOutcome(payload))
    }
    _ -> decode.failure(CleanOutcome(Clean(gates_run: 0)), "CargoGatesOutcome")
  }
}

fn gate_result_codec() -> Codec(GateResult) {
  codec.json_codec(gate_result_to_json, gate_result_decoder())
}

fn gate_result_to_json(value: GateResult) -> json.Json {
  json.object([
    #("gate", awlc.string_to_json(value.gate)),
    #("exit_code", awlc.int_to_json(value.exit_code)),
    #("passed", awlc.bool_to_json(value.passed)),
    #("output_tail", awlc.string_to_json(value.output_tail)),
  ])
}

fn gate_result_decoder() -> decode.Decoder(GateResult) {
  use gate <- decode.field("gate", awlc.string_decoder())
  use exit_code <- decode.field("exit_code", awlc.int_decoder())
  use passed <- decode.field("passed", awlc.bool_decoder())
  use output_tail <- decode.field("output_tail", awlc.string_decoder())
  decode.success(GateResult(
    gate: gate,
    exit_code: exit_code,
    passed: passed,
    output_tail: output_tail,
  ))
}

fn clean_codec() -> Codec(Clean) {
  codec.json_codec(clean_to_json, clean_decoder())
}

fn clean_to_json(value: Clean) -> json.Json {
  json.object([
    #("gates_run", awlc.int_to_json(value.gates_run)),
  ])
}

fn clean_decoder() -> decode.Decoder(Clean) {
  use gates_run <- decode.field("gates_run", awlc.int_decoder())
  decode.success(Clean(
    gates_run: gates_run,
  ))
}

fn failing_codec() -> Codec(Failing) {
  codec.json_codec(failing_to_json, failing_decoder())
}

fn failing_to_json(value: Failing) -> json.Json {
  json.object([
    #("gates", list_gate_result_to_json(value.gates)),
  ])
}

fn failing_decoder() -> decode.Decoder(Failing) {
  use gates <- decode.field("gates", list_gate_result_decoder())
  decode.success(Failing(
    gates: gates,
  ))
}

fn run_check_input_codec() -> Codec(RunCheckInput) {
  codec.json_codec(run_check_input_to_json, run_check_input_decoder())
}

fn run_check_input_to_json(value: RunCheckInput) -> json.Json {
  json.object([
    #("path", awlc.string_to_json(value.path)),
  ])
}

fn run_check_input_decoder() -> decode.Decoder(RunCheckInput) {
  use path <- decode.field("path", awlc.string_decoder())
  decode.success(RunCheckInput(
    path: path,
  ))
}

fn run_clippy_input_codec() -> Codec(RunClippyInput) {
  codec.json_codec(run_clippy_input_to_json, run_clippy_input_decoder())
}

fn run_clippy_input_to_json(value: RunClippyInput) -> json.Json {
  json.object([
    #("path", awlc.string_to_json(value.path)),
  ])
}

fn run_clippy_input_decoder() -> decode.Decoder(RunClippyInput) {
  use path <- decode.field("path", awlc.string_decoder())
  decode.success(RunClippyInput(
    path: path,
  ))
}

fn run_tests_input_codec() -> Codec(RunTestsInput) {
  codec.json_codec(run_tests_input_to_json, run_tests_input_decoder())
}

fn run_tests_input_to_json(value: RunTestsInput) -> json.Json {
  json.object([
    #("path", awlc.string_to_json(value.path)),
  ])
}

fn run_tests_input_decoder() -> decode.Decoder(RunTestsInput) {
  use path <- decode.field("path", awlc.string_decoder())
  decode.success(RunTestsInput(
    path: path,
  ))
}

fn run_fmt_check_input_codec() -> Codec(RunFmtCheckInput) {
  codec.json_codec(run_fmt_check_input_to_json, run_fmt_check_input_decoder())
}

fn run_fmt_check_input_to_json(value: RunFmtCheckInput) -> json.Json {
  json.object([
    #("path", awlc.string_to_json(value.path)),
  ])
}

fn run_fmt_check_input_decoder() -> decode.Decoder(RunFmtCheckInput) {
  use path <- decode.field("path", awlc.string_decoder())
  decode.success(RunFmtCheckInput(
    path: path,
  ))
}

fn list_gate_result_codec() -> Codec(List(GateResult)) {
  codec.json_codec(list_gate_result_to_json, list_gate_result_decoder())
}
fn list_gate_result_to_json(values: List(GateResult)) -> json.Json { json.array(values, gate_result_to_json) }
fn list_gate_result_decoder() -> decode.Decoder(List(GateResult)) { decode.list(gate_result_decoder()) }

