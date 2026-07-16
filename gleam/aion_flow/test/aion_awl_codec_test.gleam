//// Equivalence tests for the AWL-BC-0 hoisted codec glue.
////
//// The per-type record/enum/union codecs stay generated in the emitted module
//// and are assembled from the hoisted leaf codecs and the stdlib decode
//// primitives. These tests pin that assembly: for each representative shape a
//// codec is built exactly the way the emitter builds it (using the hoisted
//// `aion/awl/codec` leaves), and its wire behavior is asserted to match the
//// bytes the hand-emitted codecs produced. They also cover the hoisted
//// `AwlError` codec, the five error mappers, the run shell, and `index`.

import aion/awl/codec as awlc
import aion/awl/error.{
  type AwlError, AwlActivityFailed, AwlChildFailed, AwlDecodeInputFailed,
  AwlFailed, AwlIndexOutOfRange, AwlOutcomeFailure, AwlSignalFailed,
  AwlTimerFailed,
}
import aion/awl/runtime
import aion/codec
import aion/error as aion_error
import gleam/dynamic
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import gleeunit/should

// -- Representative type shapes, assembled the emitter's way ---------------

type Note {
  Note(name: String, memo: Option(String))
}

fn note_to_json(value: Note) -> json.Json {
  json.object(
    list.flatten([
      [#("name", awlc.string_to_json(value.name))],
      case value.memo {
        Some(inner) -> [#("memo", awlc.string_to_json(inner))]
        None -> []
      },
    ]),
  )
}

fn note_decoder() -> decode.Decoder(Note) {
  use name <- decode.field("name", awlc.string_decoder())
  use memo <- decode.optional_field(
    "memo",
    None,
    decode.map(awlc.string_decoder(), Some),
  )
  decode.success(Note(name: name, memo: memo))
}

fn note_codec() -> codec.Codec(Note) {
  codec.json_codec(note_to_json, note_decoder())
}

type Color {
  Red
  Green
}

fn color_to_json(value: Color) -> json.Json {
  case value {
    Red -> json.string("Red")
    Green -> json.string("Green")
  }
}

fn color_decoder() -> decode.Decoder(Color) {
  use tag <- decode.then(decode.string)
  case tag {
    "Red" -> decode.success(Red)
    "Green" -> decode.success(Green)
    _ -> decode.failure(Red, "Color")
  }
}

fn color_codec() -> codec.Codec(Color) {
  codec.json_codec(color_to_json, color_decoder())
}

type Outcome {
  Done(Note)
}

fn outcome_to_json(value: Outcome) -> json.Json {
  case value {
    Done(payload) ->
      json.object([
        #("outcome", json.string("done")),
        #("payload", note_to_json(payload)),
      ])
  }
}

fn outcome_decoder() -> decode.Decoder(Outcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "done" -> {
      use payload <- decode.field("payload", note_decoder())
      decode.success(Done(payload))
    }
    _ -> decode.failure(Done(Note(name: "", memo: None)), "Outcome")
  }
}

fn outcome_codec() -> codec.Codec(Outcome) {
  codec.json_codec(outcome_to_json, outcome_decoder())
}

fn notes_codec() -> codec.Codec(List(Note)) {
  codec.json_codec(
    fn(values) { json.array(values, note_to_json) },
    decode.list(note_decoder()),
  )
}

// -- Leaf codecs -----------------------------------------------------------

pub fn leaf_string_roundtrips_test() {
  awlc.string_codec().encode("hi")
  |> should.equal("\"hi\"")
  awlc.string_codec().decode("\"hi\"")
  |> should.equal(Ok("hi"))
}

pub fn leaf_int_float_bool_roundtrip_test() {
  awlc.int_codec().encode(42)
  |> should.equal("42")
  awlc.int_codec().decode("42")
  |> should.equal(Ok(42))
  awlc.float_codec().decode(awlc.float_codec().encode(1.5))
  |> should.equal(Ok(1.5))
  awlc.bool_codec().encode(True)
  |> should.equal("true")
  awlc.bool_codec().decode("false")
  |> should.equal(Ok(False))
}

pub fn leaf_nil_encodes_empty_object_test() {
  awlc.nil_codec().encode(Nil)
  |> should.equal("{}")
  // The nil decoder accepts any value.
  awlc.nil_codec().decode("123")
  |> should.equal(Ok(Nil))
}

pub fn leaf_string_handles_unicode_test() {
  let text = "héllo wörld 日本語 🎉"
  let encoded = awlc.string_codec().encode(text)
  awlc.string_codec().decode(encoded)
  |> should.equal(Ok(text))
}

// -- Records with optional fields (D4) -------------------------------------

pub fn optional_field_absent_is_omitted_on_encode_test() {
  note_codec().encode(Note(name: "a", memo: None))
  |> should.equal("{\"name\":\"a\"}")
}

pub fn optional_field_present_is_written_on_encode_test() {
  note_codec().encode(Note(name: "a", memo: Some("b")))
  |> should.equal("{\"name\":\"a\",\"memo\":\"b\"}")
}

pub fn optional_field_absent_decodes_to_none_test() {
  note_codec().decode("{\"name\":\"a\"}")
  |> should.equal(Ok(Note(name: "a", memo: None)))
}

pub fn optional_field_present_decodes_to_some_test() {
  note_codec().decode("{\"name\":\"a\",\"memo\":\"b\"}")
  |> should.equal(Ok(Note(name: "a", memo: Some("b"))))
}

pub fn optional_field_explicit_null_is_rejected_on_decode_test() {
  note_codec().decode("{\"name\":\"a\",\"memo\":null}")
  |> result.is_error
  |> should.be_true
}

// -- Enums -----------------------------------------------------------------

pub fn enum_encodes_variant_name_test() {
  color_codec().encode(Green)
  |> should.equal("\"Green\"")
}

pub fn enum_decodes_known_variant_test() {
  color_codec().decode("\"Red\"")
  |> should.equal(Ok(Red))
}

pub fn enum_unknown_variant_fails_to_decode_test() {
  color_codec().decode("\"Blue\"")
  |> result.is_error
  |> should.be_true
}

// -- Outcome unions --------------------------------------------------------

pub fn union_roundtrips_outcome_and_payload_test() {
  let value = Done(Note(name: "x", memo: Some("y")))
  outcome_codec().encode(value)
  |> should.equal(
    "{\"outcome\":\"done\",\"payload\":{\"name\":\"x\",\"memo\":\"y\"}}",
  )
  outcome_codec().decode(outcome_codec().encode(value))
  |> should.equal(Ok(value))
}

pub fn union_unknown_outcome_fails_to_decode_test() {
  outcome_codec().decode("{\"outcome\":\"nope\",\"payload\":{}}")
  |> result.is_error
  |> should.be_true
}

// -- Lists and nested composites -------------------------------------------

pub fn list_of_nested_records_roundtrips_test() {
  let values = [Note(name: "a", memo: Some("m")), Note(name: "b", memo: None)]
  notes_codec().encode(values)
  |> should.equal("[{\"name\":\"a\",\"memo\":\"m\"},{\"name\":\"b\"}]")
  notes_codec().decode(notes_codec().encode(values))
  |> should.equal(Ok(values))
}

// -- The raw / decoded / json_value glue -----------------------------------

pub fn raw_codec_passes_payload_through_test() {
  awlc.raw().encode("{\"a\":1}")
  |> should.equal("{\"a\":1}")
  awlc.raw().decode("{\"a\":1}")
  |> should.equal(Ok("{\"a\":1}"))
}

pub fn decoded_maps_success_and_failure_test() {
  awlc.decoded(note_codec(), "{\"name\":\"a\"}", "greet")
  |> should.equal(Ok(Note(name: "a", memo: None)))
  awlc.decoded(note_codec(), "not json", "greet")
  |> should.equal(Error(AwlActivityFailed("greet")))
}

pub fn json_value_codec_is_encode_only_test() {
  awlc.json_value().encode(json.object([#("k", json.int(1))]))
  |> should.equal("{\"k\":1}")
  let assert Error(codec.DecodeError(reason: reason, path: _)) =
    awlc.json_value().decode("{}")
  reason
  |> should.equal("child call input is encode-only")
}

// -- The AwlError codec ----------------------------------------------------

fn awl_error_roundtrips(value: AwlError) -> Nil {
  error.codec().decode(error.codec().encode(value))
  |> should.equal(Ok(value))
  Nil
}

pub fn awl_error_codec_roundtrips_every_variant_test() {
  awl_error_roundtrips(AwlDecodeInputFailed("m"))
  awl_error_roundtrips(AwlActivityFailed("m"))
  awl_error_roundtrips(AwlSignalFailed("m"))
  awl_error_roundtrips(AwlChildFailed("m"))
  awl_error_roundtrips(AwlTimerFailed("m"))
  awl_error_roundtrips(AwlIndexOutOfRange("m"))
  awl_error_roundtrips(AwlOutcomeFailure(outcome: "failed", payload: "{}"))
  awl_error_roundtrips(AwlFailed)
}

pub fn awl_error_wire_shape_is_stable_test() {
  error.codec().encode(AwlFailed)
  |> should.equal("{\"tag\":\"AwlFailed\"}")
  error.codec().encode(AwlOutcomeFailure(outcome: "failed", payload: "{}"))
  |> should.equal(
    "{\"tag\":\"AwlOutcomeFailure\",\"outcome\":\"failed\",\"payload\":\"{}\"}",
  )
}

pub fn awl_error_unknown_tag_fails_to_decode_test() {
  let assert Error(_) = error.codec().decode("{\"tag\":\"Nope\"}")
}

// -- The five error mappers ------------------------------------------------

pub fn map_activity_error_maps_failure_test() {
  error.map_activity_error(Error(aion_error.terminal("boom")))
  |> should.equal(Error(AwlActivityFailed("activity failed")))
  error.map_activity_error(Ok(7))
  |> should.equal(Ok(7))
}

pub fn map_receive_error_maps_failure_test() {
  error.map_receive_error(Error(aion_error.UnknownSignal("s")))
  |> should.equal(Error(AwlSignalFailed("signal receive failed")))
}

pub fn map_child_error_maps_failure_test() {
  error.map_child_error(Error(aion_error.ChildEngineFailure("boom")))
  |> should.equal(Error(AwlChildFailed("child engine failure: boom")))

  let decode_error = codec.DecodeError(reason: "Unexpected byte: 0x0", path: [])
  error.map_child_error(Error(aion_error.ChildOutputDecodeFailed(decode_error)))
  |> should.equal(
    Error(AwlChildFailed("child output decode failed: Unexpected byte: 0x0")),
  )
}

pub fn map_spawn_error_maps_failure_test() {
  error.map_spawn_error(Error(aion_error.EngineFailure("boom")))
  |> should.equal(Error(AwlChildFailed("detached spawn failed")))
}

pub fn map_timer_error_maps_failure_test() {
  error.map_timer_error(Error(aion_error.EngineFailure("boom")))
  |> should.equal(Error(AwlTimerFailed("timer failed")))
}

// -- The run shell ---------------------------------------------------------

fn shout(note: Note) -> Result(Outcome, AwlError) {
  Ok(Done(Note(name: note.name, memo: note.memo)))
}

fn run_note(raw: dynamic.Dynamic) -> Result(String, AwlError) {
  runtime.run(raw, note_codec(), outcome_codec(), shout)
}

pub fn run_success_encodes_output_test() {
  run_note(dynamic.string(note_codec().encode(Note(name: "a", memo: None))))
  |> should.equal(Ok("{\"outcome\":\"done\",\"payload\":{\"name\":\"a\"}}"))
}

pub fn run_non_string_payload_fails_test() {
  run_note(dynamic.int(42))
  |> should.equal(
    Error(AwlDecodeInputFailed("workflow input payload was not a string")),
  )
}

pub fn run_undecodable_input_fails_test() {
  let assert Error(AwlDecodeInputFailed(message)) =
    run_note(dynamic.string("{not json"))
  string.starts_with(message, "failed to decode workflow input: ")
  |> should.be_true
}

pub fn run_propagates_execute_error_test() {
  let boom = fn(_note: Note) -> Result(Outcome, AwlError) {
    Error(AwlOutcomeFailure(outcome: "failed", payload: "{}"))
  }
  runtime.run(
    dynamic.string(note_codec().encode(Note(name: "a", memo: None))),
    note_codec(),
    outcome_codec(),
    boom,
  )
  |> should.equal(Error(AwlOutcomeFailure(outcome: "failed", payload: "{}")))
}

// -- index -----------------------------------------------------------------

pub fn index_in_range_returns_value_test() {
  runtime.index([10, 20, 30], 1, "out of range")
  |> should.equal(Ok(20))
}

pub fn index_out_of_range_is_a_step_failure_test() {
  runtime.index([10, 20], 5, "label text")
  |> should.equal(Error(AwlIndexOutOfRange("label text")))
}
