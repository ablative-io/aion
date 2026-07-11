//// Fixed codec glue hoisted out of every generated AWL module (AWL-BC-0,
//// hoist-only): the builtin leaf codecs (`String`/`Int`/`Float`/`Bool`/`Nil`),
//// the raw wire-passthrough codec heterogeneous parallel branches ride, the
//// per-branch decode helper, and the encode-only child-input codec.
////
//// Generated per-type record/enum/union codecs stay in the emitted module and
//// reference these leaves qualified (`awlc.string_to_json`, …); the behavior is
//// byte-identical to the code the emitter used to inline.

import aion/awl/error.{type AwlError, AwlActivityFailed}
import aion/codec.{type Codec}
import gleam/dynamic/decode
import gleam/json

// -- Builtin leaf codecs ---------------------------------------------------

/// `String` codec.
pub fn string_codec() -> Codec(String) {
  codec.json_codec(json.string, decode.string)
}

/// `Int` codec.
pub fn int_codec() -> Codec(Int) {
  codec.json_codec(json.int, decode.int)
}

/// `Float` codec.
pub fn float_codec() -> Codec(Float) {
  codec.json_codec(json.float, decode.float)
}

/// `Bool` codec.
pub fn bool_codec() -> Codec(Bool) {
  codec.json_codec(json.bool, decode.bool)
}

/// `Nil` codec: encodes `{}` and decodes accepting any value.
pub fn nil_codec() -> Codec(Nil) {
  codec.json_codec(fn(_) { json.object([]) }, decode.success(Nil))
}

/// `String` JSON encoder.
pub fn string_to_json(value: String) -> json.Json {
  json.string(value)
}

/// `Int` JSON encoder.
pub fn int_to_json(value: Int) -> json.Json {
  json.int(value)
}

/// `Float` JSON encoder.
pub fn float_to_json(value: Float) -> json.Json {
  json.float(value)
}

/// `Bool` JSON encoder.
pub fn bool_to_json(value: Bool) -> json.Json {
  json.bool(value)
}

/// `Nil` JSON encoder: the empty object.
pub fn nil_to_json(_: Nil) -> json.Json {
  json.object([])
}

/// `String` decoder.
pub fn string_decoder() -> decode.Decoder(String) {
  decode.string
}

/// `Int` decoder.
pub fn int_decoder() -> decode.Decoder(Int) {
  decode.int
}

/// `Float` decoder.
pub fn float_decoder() -> decode.Decoder(Float) {
  decode.float
}

/// `Bool` decoder.
pub fn bool_decoder() -> decode.Decoder(Bool) {
  decode.bool
}

/// `Nil` decoder: succeeds on any value.
pub fn nil_decoder() -> decode.Decoder(Nil) {
  decode.success(Nil)
}

// -- Parallel-branch and child-input glue ----------------------------------

/// Identity codec: heterogeneous parallel branches ride `workflow.all` as raw
/// JSON payload strings, decoded per branch at the join with [`decoded`].
pub fn raw() -> Codec(String) {
  codec.Codec(encode: fn(payload) { payload }, decode: fn(payload) {
    Ok(payload)
  })
}

/// Decode one parallel branch's payload with its action's return codec; a
/// decode failure becomes a step failure naming the action.
pub fn decoded(
  item_codec: Codec(a),
  payload: String,
  action: String,
) -> Result(a, AwlError) {
  case item_codec.decode(payload) {
    Ok(value) -> Ok(value)
    Error(_) -> Error(AwlActivityFailed(action))
  }
}

/// Encode-only codec for child workflow inputs: the parent assembles the
/// child's input record as JSON and never decodes it back.
pub fn json_value() -> Codec(json.Json) {
  codec.Codec(encode: json.to_string, decode: fn(_) {
    Error(codec.DecodeError(reason: "child call input is encode-only", path: []))
  })
}
