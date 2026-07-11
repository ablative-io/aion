//// awl_sitting: one exam sitting, machine-invigilated — provision a scratch room,
//// run the candidate, grade the paper, collect turn-2 feedback, hand back one row.
//// A sitting never kills the suite: crashes become Never-class rows (exit-status-is-data).

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
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result

pub type SittingSpec {
  SittingSpec(
    harness: String,
    model: String,
    effort: String,
  )
}

pub type Scratch {
  Scratch(
    path: String,
  )
}

pub type Submission {
  Submission(
    file_path: String,
    transcript_path: String,
    said_done: Bool,
  )
}

/// first_try | after_fixes | never — the exam protocol's check-pass mark.
pub type CheckClass {
  FirstTry
  AfterFixes
  Never
}

pub type CheckResult {
  CheckResult(
    class: CheckClass,
    first_try: Bool,
    fix_rounds: Int,
    errors: List(String),
  )
}

pub type SemanticMark {
  SemanticMark(
    requirement: Int,
    passed: Bool,
    note: Option(String),
  )
}

pub type Feedback {
  Feedback(
    confidence: Float,
    confusions: List(String),
    missing: List(String),
  )
}

pub type SittingRow {
  SittingRow(
    spec: SittingSpec,
    check: CheckResult,
    marks: List(SemanticMark),
    feedback: Feedback,
    crashed: Bool,
    first_try: Bool,
  )
}

pub type AwlSittingInput {
  AwlSittingInput(
    spec: SittingSpec,
    pack_revision: String,
  )
}

pub type AwlSittingOutcome {
  RowOutcome(SittingRow)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(AwlSittingInput, AwlSittingOutcome, awl_error.AwlError) {
  workflow.define(
    "awl_sitting",
    awl_sitting_input_codec(),
    awl_sitting_outcome_codec(),
    awl_error.codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, awl_error.AwlError) {
  runtime.run(raw_input, awl_sitting_input_codec(), awl_sitting_outcome_codec(), execute)
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: AwlSittingInput) -> Result(AwlSittingOutcome, awl_error.AwlError) {
  let spec = input.spec
  let pack_revision = input.pack_revision
  step_provision(pack_revision, spec)
}

fn step_provision(pack_revision: String, spec: SittingSpec) -> Result(AwlSittingOutcome, awl_error.AwlError) {
  use scratch <- result.try(provision_scratch_activity(spec, pack_revision) |> activity.retry(activity.RetryPolicy(max_attempts: 2, backoff: activity.Fixed(duration.milliseconds(30000)))) |> activity.timeout(duration.milliseconds(120000)) |> activity.task_queue("awl_exam") |> activity.node("shell") |> workflow.run |> awl_error.map_activity_error)
  let awl_attempt = fn() {
    use submission <- result.try(run_candidate_activity(spec, scratch) |> activity.timeout(duration.milliseconds(2700000)) |> activity.task_queue("awl_exam") |> activity.node("candidate") |> workflow.run |> awl_error.map_activity_error)
    Ok(submission)
  }
  case awl_attempt() {
    Ok(submission) -> {
      use check <- result.try(grade_check_activity(submission, scratch) |> activity.timeout(duration.milliseconds(600000)) |> activity.task_queue("awl_exam") |> activity.node("shell") |> workflow.run |> awl_error.map_activity_error)
      use marks <- result.try(grade_semantics_activity(submission) |> activity.timeout(duration.milliseconds(900000)) |> activity.task_queue("awl_exam") |> activity.node("grader") |> workflow.run |> awl_error.map_activity_error)
      use feedback <- result.try(collect_feedback_activity(spec, submission, check) |> activity.timeout(duration.milliseconds(900000)) |> activity.task_queue("awl_exam") |> activity.node("candidate") |> workflow.run |> awl_error.map_activity_error)
      Ok(RowOutcome(SittingRow(spec: spec, check: check, marks: marks, feedback: feedback, crashed: False, first_try: check.first_try)))
    }
    Error(_) -> {
      Ok(RowOutcome(SittingRow(spec: spec, check: CheckResult(class: Never, first_try: False, fix_rounds: 0, errors: []), marks: [], feedback: Feedback(confidence: 0.0, confusions: [], missing: []), crashed: True, first_try: False)))
    }
  }
}

pub type ProvisionScratchInput {
  ProvisionScratchInput(
    spec: SittingSpec,
    pack_revision: String,
  )
}

fn provision_scratch_activity(
  spec: SittingSpec,
  pack_revision: String,
) -> activity.Activity(ProvisionScratchInput, Scratch) {
  activity.new(
    "provision_scratch",
    ProvisionScratchInput(
      spec: spec,
      pack_revision: pack_revision,
    ),
    provision_scratch_input_codec(),
    scratch_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type RunCandidateInput {
  RunCandidateInput(
    spec: SittingSpec,
    scratch: Scratch,
  )
}

fn run_candidate_activity(
  spec: SittingSpec,
  scratch: Scratch,
) -> activity.Activity(RunCandidateInput, Submission) {
  activity.new(
    "run_candidate",
    RunCandidateInput(
      spec: spec,
      scratch: scratch,
    ),
    run_candidate_input_codec(),
    submission_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type GradeCheckInput {
  GradeCheckInput(
    submission: Submission,
    scratch: Scratch,
  )
}

fn grade_check_activity(
  submission: Submission,
  scratch: Scratch,
) -> activity.Activity(GradeCheckInput, CheckResult) {
  activity.new(
    "grade_check",
    GradeCheckInput(
      submission: submission,
      scratch: scratch,
    ),
    grade_check_input_codec(),
    check_result_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type GradeSemanticsInput {
  GradeSemanticsInput(
    submission: Submission,
  )
}

fn grade_semantics_activity(
  submission: Submission,
) -> activity.Activity(GradeSemanticsInput, List(SemanticMark)) {
  activity.new(
    "grade_semantics",
    GradeSemanticsInput(
      submission: submission,
    ),
    grade_semantics_input_codec(),
    list_semantic_mark_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type CollectFeedbackInput {
  CollectFeedbackInput(
    spec: SittingSpec,
    submission: Submission,
    check: CheckResult,
  )
}

fn collect_feedback_activity(
  spec: SittingSpec,
  submission: Submission,
  check: CheckResult,
) -> activity.Activity(CollectFeedbackInput, Feedback) {
  activity.new(
    "collect_feedback",
    CollectFeedbackInput(
      spec: spec,
      submission: submission,
      check: check,
    ),
    collect_feedback_input_codec(),
    feedback_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn awl_sitting_input_codec() -> Codec(AwlSittingInput) {
  codec.json_codec(awl_sitting_input_to_json, awl_sitting_input_decoder())
}

fn awl_sitting_input_to_json(value: AwlSittingInput) -> json.Json {
  json.object([
    #("spec", sitting_spec_to_json(value.spec)),
    #("pack_revision", awlc.string_to_json(value.pack_revision)),
  ])
}

fn awl_sitting_input_decoder() -> decode.Decoder(AwlSittingInput) {
  use spec <- decode.field("spec", sitting_spec_decoder())
  use pack_revision <- decode.field("pack_revision", awlc.string_decoder())
  decode.success(AwlSittingInput(
    spec: spec,
    pack_revision: pack_revision,
  ))
}

fn awl_sitting_outcome_codec() -> Codec(AwlSittingOutcome) {
  codec.json_codec(awl_sitting_outcome_to_json, awl_sitting_outcome_decoder())
}

fn awl_sitting_outcome_to_json(value: AwlSittingOutcome) -> json.Json {
  case value {
    RowOutcome(payload) -> json.object([#("outcome", json.string("row")), #("payload", sitting_row_to_json(payload))])
  }
}

fn awl_sitting_outcome_decoder() -> decode.Decoder(AwlSittingOutcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "row" -> {
      use payload <- decode.field("payload", sitting_row_decoder())
      decode.success(RowOutcome(payload))
    }
    _ -> decode.failure(RowOutcome(SittingRow(spec: SittingSpec(harness: "", model: "", effort: ""), check: CheckResult(class: FirstTry, first_try: False, fix_rounds: 0, errors: []), marks: [], feedback: Feedback(confidence: 0.0, confusions: [], missing: []), crashed: False, first_try: False)), "AwlSittingOutcome")
  }
}

fn sitting_spec_codec() -> Codec(SittingSpec) {
  codec.json_codec(sitting_spec_to_json, sitting_spec_decoder())
}

fn sitting_spec_to_json(value: SittingSpec) -> json.Json {
  json.object([
    #("harness", awlc.string_to_json(value.harness)),
    #("model", awlc.string_to_json(value.model)),
    #("effort", awlc.string_to_json(value.effort)),
  ])
}

fn sitting_spec_decoder() -> decode.Decoder(SittingSpec) {
  use harness <- decode.field("harness", awlc.string_decoder())
  use model <- decode.field("model", awlc.string_decoder())
  use effort <- decode.field("effort", awlc.string_decoder())
  decode.success(SittingSpec(
    harness: harness,
    model: model,
    effort: effort,
  ))
}

fn scratch_codec() -> Codec(Scratch) {
  codec.json_codec(scratch_to_json, scratch_decoder())
}

fn scratch_to_json(value: Scratch) -> json.Json {
  json.object([
    #("path", awlc.string_to_json(value.path)),
  ])
}

fn scratch_decoder() -> decode.Decoder(Scratch) {
  use path <- decode.field("path", awlc.string_decoder())
  decode.success(Scratch(
    path: path,
  ))
}

fn submission_codec() -> Codec(Submission) {
  codec.json_codec(submission_to_json, submission_decoder())
}

fn submission_to_json(value: Submission) -> json.Json {
  json.object([
    #("file_path", awlc.string_to_json(value.file_path)),
    #("transcript_path", awlc.string_to_json(value.transcript_path)),
    #("said_done", awlc.bool_to_json(value.said_done)),
  ])
}

fn submission_decoder() -> decode.Decoder(Submission) {
  use file_path <- decode.field("file_path", awlc.string_decoder())
  use transcript_path <- decode.field("transcript_path", awlc.string_decoder())
  use said_done <- decode.field("said_done", awlc.bool_decoder())
  decode.success(Submission(
    file_path: file_path,
    transcript_path: transcript_path,
    said_done: said_done,
  ))
}

fn check_class_codec() -> Codec(CheckClass) {
  codec.json_codec(check_class_to_json, check_class_decoder())
}

fn check_class_to_json(value: CheckClass) -> json.Json {
  case value {
    FirstTry -> json.string("FirstTry")
    AfterFixes -> json.string("AfterFixes")
    Never -> json.string("Never")
  }
}

fn check_class_decoder() -> decode.Decoder(CheckClass) {
  use value <- decode.then(decode.string)
  case value {
    "FirstTry" -> decode.success(FirstTry)
    "AfterFixes" -> decode.success(AfterFixes)
    "Never" -> decode.success(Never)
    _ -> decode.failure(FirstTry, "CheckClass")
  }
}

fn check_result_codec() -> Codec(CheckResult) {
  codec.json_codec(check_result_to_json, check_result_decoder())
}

fn check_result_to_json(value: CheckResult) -> json.Json {
  json.object([
    #("class", check_class_to_json(value.class)),
    #("first_try", awlc.bool_to_json(value.first_try)),
    #("fix_rounds", awlc.int_to_json(value.fix_rounds)),
    #("errors", list_string_to_json(value.errors)),
  ])
}

fn check_result_decoder() -> decode.Decoder(CheckResult) {
  use class <- decode.field("class", check_class_decoder())
  use first_try <- decode.field("first_try", awlc.bool_decoder())
  use fix_rounds <- decode.field("fix_rounds", awlc.int_decoder())
  use errors <- decode.field("errors", list_string_decoder())
  decode.success(CheckResult(
    class: class,
    first_try: first_try,
    fix_rounds: fix_rounds,
    errors: errors,
  ))
}

fn semantic_mark_codec() -> Codec(SemanticMark) {
  codec.json_codec(semantic_mark_to_json, semantic_mark_decoder())
}

fn semantic_mark_to_json(value: SemanticMark) -> json.Json {
  json.object(list.flatten([
    [#("requirement", awlc.int_to_json(value.requirement))],
    [#("passed", awlc.bool_to_json(value.passed))],
    case value.note {
      Some(inner) -> [#("note", awlc.string_to_json(inner))]
      None -> []
    },
  ]))
}

fn semantic_mark_decoder() -> decode.Decoder(SemanticMark) {
  use requirement <- decode.field("requirement", awlc.int_decoder())
  use passed <- decode.field("passed", awlc.bool_decoder())
  use note <- decode.optional_field("note", None, decode.map(awlc.string_decoder(), Some))
  decode.success(SemanticMark(
    requirement: requirement,
    passed: passed,
    note: note,
  ))
}

fn feedback_codec() -> Codec(Feedback) {
  codec.json_codec(feedback_to_json, feedback_decoder())
}

fn feedback_to_json(value: Feedback) -> json.Json {
  json.object([
    #("confidence", awlc.float_to_json(value.confidence)),
    #("confusions", list_string_to_json(value.confusions)),
    #("missing", list_string_to_json(value.missing)),
  ])
}

fn feedback_decoder() -> decode.Decoder(Feedback) {
  use confidence <- decode.field("confidence", awlc.float_decoder())
  use confusions <- decode.field("confusions", list_string_decoder())
  use missing <- decode.field("missing", list_string_decoder())
  decode.success(Feedback(
    confidence: confidence,
    confusions: confusions,
    missing: missing,
  ))
}

fn sitting_row_codec() -> Codec(SittingRow) {
  codec.json_codec(sitting_row_to_json, sitting_row_decoder())
}

fn sitting_row_to_json(value: SittingRow) -> json.Json {
  json.object([
    #("spec", sitting_spec_to_json(value.spec)),
    #("check", check_result_to_json(value.check)),
    #("marks", list_semantic_mark_to_json(value.marks)),
    #("feedback", feedback_to_json(value.feedback)),
    #("crashed", awlc.bool_to_json(value.crashed)),
    #("first_try", awlc.bool_to_json(value.first_try)),
  ])
}

fn sitting_row_decoder() -> decode.Decoder(SittingRow) {
  use spec <- decode.field("spec", sitting_spec_decoder())
  use check <- decode.field("check", check_result_decoder())
  use marks <- decode.field("marks", list_semantic_mark_decoder())
  use feedback <- decode.field("feedback", feedback_decoder())
  use crashed <- decode.field("crashed", awlc.bool_decoder())
  use first_try <- decode.field("first_try", awlc.bool_decoder())
  decode.success(SittingRow(
    spec: spec,
    check: check,
    marks: marks,
    feedback: feedback,
    crashed: crashed,
    first_try: first_try,
  ))
}

fn provision_scratch_input_codec() -> Codec(ProvisionScratchInput) {
  codec.json_codec(provision_scratch_input_to_json, provision_scratch_input_decoder())
}

fn provision_scratch_input_to_json(value: ProvisionScratchInput) -> json.Json {
  json.object([
    #("spec", sitting_spec_to_json(value.spec)),
    #("pack_revision", awlc.string_to_json(value.pack_revision)),
  ])
}

fn provision_scratch_input_decoder() -> decode.Decoder(ProvisionScratchInput) {
  use spec <- decode.field("spec", sitting_spec_decoder())
  use pack_revision <- decode.field("pack_revision", awlc.string_decoder())
  decode.success(ProvisionScratchInput(
    spec: spec,
    pack_revision: pack_revision,
  ))
}

fn run_candidate_input_codec() -> Codec(RunCandidateInput) {
  codec.json_codec(run_candidate_input_to_json, run_candidate_input_decoder())
}

fn run_candidate_input_to_json(value: RunCandidateInput) -> json.Json {
  json.object([
    #("spec", sitting_spec_to_json(value.spec)),
    #("scratch", scratch_to_json(value.scratch)),
  ])
}

fn run_candidate_input_decoder() -> decode.Decoder(RunCandidateInput) {
  use spec <- decode.field("spec", sitting_spec_decoder())
  use scratch <- decode.field("scratch", scratch_decoder())
  decode.success(RunCandidateInput(
    spec: spec,
    scratch: scratch,
  ))
}

fn grade_check_input_codec() -> Codec(GradeCheckInput) {
  codec.json_codec(grade_check_input_to_json, grade_check_input_decoder())
}

fn grade_check_input_to_json(value: GradeCheckInput) -> json.Json {
  json.object([
    #("submission", submission_to_json(value.submission)),
    #("scratch", scratch_to_json(value.scratch)),
  ])
}

fn grade_check_input_decoder() -> decode.Decoder(GradeCheckInput) {
  use submission <- decode.field("submission", submission_decoder())
  use scratch <- decode.field("scratch", scratch_decoder())
  decode.success(GradeCheckInput(
    submission: submission,
    scratch: scratch,
  ))
}

fn grade_semantics_input_codec() -> Codec(GradeSemanticsInput) {
  codec.json_codec(grade_semantics_input_to_json, grade_semantics_input_decoder())
}

fn grade_semantics_input_to_json(value: GradeSemanticsInput) -> json.Json {
  json.object([
    #("submission", submission_to_json(value.submission)),
  ])
}

fn grade_semantics_input_decoder() -> decode.Decoder(GradeSemanticsInput) {
  use submission <- decode.field("submission", submission_decoder())
  decode.success(GradeSemanticsInput(
    submission: submission,
  ))
}

fn collect_feedback_input_codec() -> Codec(CollectFeedbackInput) {
  codec.json_codec(collect_feedback_input_to_json, collect_feedback_input_decoder())
}

fn collect_feedback_input_to_json(value: CollectFeedbackInput) -> json.Json {
  json.object([
    #("spec", sitting_spec_to_json(value.spec)),
    #("submission", submission_to_json(value.submission)),
    #("check", check_result_to_json(value.check)),
  ])
}

fn collect_feedback_input_decoder() -> decode.Decoder(CollectFeedbackInput) {
  use spec <- decode.field("spec", sitting_spec_decoder())
  use submission <- decode.field("submission", submission_decoder())
  use check <- decode.field("check", check_result_decoder())
  decode.success(CollectFeedbackInput(
    spec: spec,
    submission: submission,
    check: check,
  ))
}

fn list_semantic_mark_codec() -> Codec(List(SemanticMark)) {
  codec.json_codec(list_semantic_mark_to_json, list_semantic_mark_decoder())
}
fn list_semantic_mark_to_json(values: List(SemanticMark)) -> json.Json { json.array(values, semantic_mark_to_json) }
fn list_semantic_mark_decoder() -> decode.Decoder(List(SemanticMark)) { decode.list(semantic_mark_decoder()) }

fn list_string_codec() -> Codec(List(String)) {
  codec.json_codec(list_string_to_json, list_string_decoder())
}
fn list_string_to_json(values: List(String)) -> json.Json { json.array(values, awlc.string_to_json) }
fn list_string_decoder() -> decode.Decoder(List(String)) { decode.list(awlc.string_decoder()) }

fn option_string_codec() -> Codec(Option(String)) {
  codec.json_codec(option_string_to_json, option_string_decoder())
}
fn option_string_to_json(value: Option(String)) -> json.Json { json.nullable(value, awlc.string_to_json) }
fn option_string_decoder() -> decode.Decoder(Option(String)) { decode.optional(awlc.string_decoder()) }

