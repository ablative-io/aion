//// Typed signal references plus receive/send wrappers.

import aion/codec.{type Codec}
import aion/error
import aion/internal/ffi
import aion/internal/pump
import gleam/json
import gleam/string

/// A typed reference to a named workflow signal.
///
/// `payload` is the statically-known payload type. The codec is carried with the
/// reference so receive and send can cross the type-erased engine boundary
/// without asking workflow authors to hand-encode values at call sites.
pub opaque type SignalRef(payload) {
  SignalRef(name: String, codec: Codec(payload))
}

/// Construct a typed signal reference.
pub fn new(name: String, payload_codec: Codec(payload)) -> SignalRef(payload) {
  SignalRef(name: name, codec: payload_codec)
}

/// Return the signal name used by the engine signal router boundary.
pub fn name(reference: SignalRef(payload)) -> String {
  reference.name
}

/// Return the payload codec carried by a signal reference.
pub fn codec(reference: SignalRef(payload)) -> Codec(payload) {
  reference.codec
}

/// Receive the next payload for a typed signal reference.
///
/// The actual selective-receive and replay behavior is owned by AT/AD. This SDK
/// wrapper binds to that router through `aion/internal/ffi`, then decodes the
/// recorded payload with the reference codec and returns decode failures as
/// typed data. The await is a yield point: pending workflow queries are
/// serviced by the query pump before the signal resolves.
pub fn receive(
  reference: SignalRef(payload),
) -> Result(payload, error.ReceiveError) {
  // Both arguments are precomputed so the pump thunk's body is exactly one
  // shielded FFI call on captured values — the re-execution-safety contract
  // for suspending awaits (see `aion/internal/pump`).
  let signal_name = name(reference)
  let config = receive_config()
  case pump.run(fn() { pump.shield(ffi.receive_signal(signal_name, config)) }) {
    Ok(raw_payload) -> {
      let payload_codec = codec(reference)
      case payload_codec.decode(raw_payload) {
        Ok(payload) -> Ok(payload)
        Error(decode_error) -> Error(error.ReceiveDecodeFailed(decode_error))
      }
    }
    Error(raw_error) -> Error(receive_error(raw_error))
  }
}

/// Send a typed signal payload to a workflow through the in-engine/Gleam-client
/// binding.
///
/// This helper encodes the payload with the `SignalRef` codec and calls AT's
/// signal-delivery boundary through FFI. It is not an HTTP or network client;
/// network-facing clients are provided outside `aion_flow`.
pub fn send(
  workflow_id: String,
  reference: SignalRef(payload),
  payload: payload,
) -> Result(Nil, error.EngineError) {
  let payload_codec = codec(reference)
  let encoded_payload = payload_codec.encode(payload)

  case ffi.send_signal(workflow_id, name(reference), encoded_payload) {
    Ok(_) -> Ok(Nil)
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

fn receive_config() -> String {
  json.object([]) |> json.to_string
}

fn receive_error(raw: String) -> error.ReceiveError {
  case string.starts_with(raw, "unknown:") {
    True -> error.UnknownSignal(name: string.drop_start(raw, 8))
    False ->
      case string.starts_with(raw, "cancelled:") {
        True ->
          error.ReceiveCancelled(error.Cancelled(string.drop_start(raw, 10)))
        False ->
          case string.starts_with(raw, "non_determinism:") {
            True ->
              error.ReceiveNonDeterministic(
                error.NonDeterminismViolation(string.drop_start(raw, 16)),
              )
            False -> error.ReceiveEngineFailure(message: raw)
          }
      }
  }
}
