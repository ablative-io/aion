//// Typed JSON encoder/decoder helpers and raw Payload escape hatch.

import aion_client/error.{type Error}
import gleam/dynamic/decode
import gleam/json

pub const json_content_type = "application/json"

/// Opaque bytes plus content-type, matching the AW Payload wire shape. Gleam's
/// public SDK keeps the byte data in a String so JSON payloads and conformance
/// fixtures remain directly inspectable while still preserving the raw escape
/// hatch and content-type boundary.
pub type Payload {
  Payload(content_type: String, bytes: String)
}

/// Encode a typed value as a JSON Payload.
pub fn encode(value: value, encoder: fn(value) -> json.Json) -> Payload {
  Payload(
    content_type: json_content_type,
    bytes: value |> encoder |> json.to_string,
  )
}

/// Decode a JSON Payload into a typed value. Decode failures are explicit data
/// and map to InvalidArgument rather than panicking or silently defaulting.
pub fn decode(
  payload: Payload,
  decoder: decode.Decoder(value),
) -> Result(value, Error) {
  let Payload(content_type: content_type, bytes: bytes) = payload

  case content_type == json_content_type {
    True -> bytes |> json.parse(decoder) |> map_decode_error
    False -> Error(error.InvalidArgument)
  }
}

fn map_decode_error(
  result: Result(value, json.DecodeError),
) -> Result(value, Error) {
  case result {
    Ok(value) -> Ok(value)
    Error(_) -> Error(error.InvalidArgument)
  }
}
