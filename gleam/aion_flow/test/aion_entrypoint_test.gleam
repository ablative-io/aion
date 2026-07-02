//// Tests for `workflow.entrypoint`: the engine-facing run adapter assembled
//// from a `WorkflowDefinition`'s codecs and typed entry function.
////
//// The load-bearing assertions are byte-equality with the hand-written
//// adapter every workflow used to carry (the gate.gleam shape): success and
//// typed failure must encode to the exact same payload text, because the
//// engine records those bytes as the run terminal and an awaiting parent
//// decodes them with the same codecs. The garbage-input edge is pinned to the
//// documented `{"aion_error":"input_decode","reason":...,"path":[...]}`
//// envelope.

import aion/codec
import aion/workflow
import gleam/dynamic
import gleam/dynamic/decode
import gleam/json
import gleeunit/should

pub type Job {
  Job(id: String, retries: Int)
}

pub type Receipt {
  Receipt(id: String, done: Bool)
}

pub type JobError {
  JobStageFailed(stage: String, message: String)
}

fn job_codec() -> codec.Codec(Job) {
  codec.json_codec(
    fn(job: Job) {
      json.object([
        #("id", json.string(job.id)),
        #("retries", json.int(job.retries)),
      ])
    },
    {
      use id <- decode.field("id", decode.string)
      use retries <- decode.field("retries", decode.int)
      decode.success(Job(id: id, retries: retries))
    },
  )
}

fn receipt_codec() -> codec.Codec(Receipt) {
  codec.json_codec(
    fn(receipt: Receipt) {
      json.object([
        #("id", json.string(receipt.id)),
        #("done", json.bool(receipt.done)),
      ])
    },
    {
      use id <- decode.field("id", decode.string)
      use done <- decode.field("done", decode.bool)
      decode.success(Receipt(id: id, done: done))
    },
  )
}

fn job_error_codec() -> codec.Codec(JobError) {
  codec.json_codec(
    fn(job_error: JobError) {
      let JobStageFailed(stage: stage, message: message) = job_error
      json.object([
        #("stage", json.string(stage)),
        #("message", json.string(message)),
      ])
    },
    {
      use stage <- decode.field("stage", decode.string)
      use message <- decode.field("message", decode.string)
      decode.success(JobStageFailed(stage: stage, message: message))
    },
  )
}

/// A typed entry that succeeds on non-negative retries and fails typed
/// otherwise, exercising both encoded sides from one definition.
fn execute(job: Job) -> Result(Receipt, JobError) {
  case job.retries >= 0 {
    True -> Ok(Receipt(id: job.id, done: True))
    False ->
      Error(JobStageFailed(stage: "validate", message: "negative retries"))
  }
}

fn definition() -> workflow.WorkflowDefinition(Job, Receipt, JobError) {
  workflow.define(
    "entrypoint_job",
    job_codec(),
    receipt_codec(),
    job_error_codec(),
    execute,
  )
}

/// The hand-written adapter shape `entrypoint` replaces (gate.gleam's run),
/// kept here as the byte-equality oracle for the success and typed-error
/// paths.
fn hand_written_run(raw_input: dynamic.Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case job_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) -> Ok(receipt_codec().encode(output))
            Error(job_error) -> Error(job_error_codec().encode(job_error))
          }
        Error(_) -> Error("hand-written adapters shaped this edge themselves")
      }
    Error(_) -> Error("hand-written adapters shaped this edge themselves")
  }
}

pub fn entrypoint_success_round_trips_through_the_codecs_test() {
  let raw = dynamic.string(job_codec().encode(Job(id: "job-1", retries: 2)))

  workflow.entrypoint(definition(), raw)
  |> should.equal(Ok(receipt_codec().encode(Receipt(id: "job-1", done: True))))
}

pub fn entrypoint_success_is_byte_identical_to_the_hand_written_adapter_test() {
  let raw = dynamic.string(job_codec().encode(Job(id: "job-2", retries: 0)))

  workflow.entrypoint(definition(), raw)
  |> should.equal(hand_written_run(raw))
}

pub fn entrypoint_typed_error_encodes_via_the_error_codec_test() {
  let raw = dynamic.string(job_codec().encode(Job(id: "job-3", retries: -1)))

  workflow.entrypoint(definition(), raw)
  |> should.equal(
    Error(
      job_error_codec().encode(JobStageFailed(
        stage: "validate",
        message: "negative retries",
      )),
    ),
  )
  workflow.entrypoint(definition(), raw)
  |> should.equal(hand_written_run(raw))
}

pub fn non_string_input_yields_the_input_decode_envelope_test() {
  let assert Error(payload) = workflow.entrypoint(definition(), dynamic.int(42))

  payload
  |> should.equal(
    json.to_string(
      json.object([
        #("aion_error", json.string("input_decode")),
        #("reason", json.string("workflow input payload was not a string")),
        #("path", json.array([], json.string)),
      ]),
    ),
  )
}

pub fn malformed_json_yields_the_envelope_with_a_reason_test() {
  let assert Error(payload) =
    workflow.entrypoint(definition(), dynamic.string("{not json"))

  let assert Ok(#(aion_error, reason, path)) = parse_envelope(payload)
  aion_error |> should.equal("input_decode")
  reason |> should.not_equal("")
  path |> should.equal([])
}

pub fn schema_mismatch_yields_the_envelope_with_reason_and_path_test() {
  // Valid JSON, wrong shape: `retries` is a string, not an int.
  let mismatched = "{\"id\":\"job-4\",\"retries\":\"three\"}"
  let assert Error(payload) =
    workflow.entrypoint(definition(), dynamic.string(mismatched))

  let assert Ok(#(aion_error, reason, path)) = parse_envelope(payload)
  aion_error |> should.equal("input_decode")
  reason |> should.equal("Expected Int, found String")
  path |> should.equal(["retries"])
}

fn parse_envelope(
  payload: String,
) -> Result(#(String, String, List(String)), json.DecodeError) {
  json.parse(payload, {
    use aion_error <- decode.field("aion_error", decode.string)
    use reason <- decode.field("reason", decode.string)
    use path <- decode.field("path", decode.list(decode.string))
    decode.success(#(aion_error, reason, path))
  })
}
