//// Typed codecs for values crossing the Aion workflow boundary.

import gleam/dynamic/decode
import gleam/json

/// A typed encoder/decoder pair over the string form consumed by the FFI
/// boundary.
///
/// Encoders produce the canonical string payload sent to the engine. Decoders
/// turn that string payload back into the expected Gleam value and report a
/// typed `DecodeError` on malformed input or schema mismatch.
pub type Codec(a) {
  Codec(encode: fn(a) -> String, decode: fn(String) -> Result(a, DecodeError))
}

/// A typed boundary decode failure.
///
/// `reason` describes the failing expectation and `path` points at the nested
/// JSON field or index when the underlying decoder can provide one.
pub type DecodeError {
  DecodeError(reason: String, path: List(String))
}

/// Build a `Codec` from a `gleam_json` encoder and decoder.
///
/// Malformed JSON and decoder mismatches are mapped to `DecodeError` values;
/// decode failures are returned as data.
pub fn json_codec(
  encoder: fn(a) -> json.Json,
  decoder: decode.Decoder(a),
) -> Codec(a) {
  Codec(
    encode: fn(value) { value |> encoder |> json.to_string },
    decode: fn(input) {
      input
      |> json.parse(decoder)
      |> result_map_error(json_decode_error)
    },
  )
}

fn json_decode_error(error: json.DecodeError) -> DecodeError {
  case error {
    json.UnexpectedEndOfInput -> DecodeError("Unexpected end of input", [])
    json.UnexpectedByte(byte) -> DecodeError("Unexpected byte: " <> byte, [])
    json.UnexpectedSequence(sequence) ->
      DecodeError("Unexpected sequence: " <> sequence, [])
    json.UnableToDecode(errors) -> dynamic_decode_error(errors)
  }
}

fn dynamic_decode_error(errors: List(decode.DecodeError)) -> DecodeError {
  case errors {
    [] -> DecodeError("Unable to decode value", [])
    [decode.DecodeError(expected: expected, found: found, path: path), ..] ->
      DecodeError("Expected " <> expected <> ", found " <> found, path)
  }
}

fn result_map_error(
  result: Result(a, error),
  mapper: fn(error) -> mapped_error,
) -> Result(a, mapped_error) {
  case result {
    Ok(value) -> Ok(value)
    Error(error) -> Error(mapper(error))
  }
}
