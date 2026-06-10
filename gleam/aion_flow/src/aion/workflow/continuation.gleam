//// continue_as_new workflow primitive.

import aion/codec.{type Codec}
import aion/error
import aion/internal/ffi

/// Continue the current workflow as a new run with fresh history.
///
/// The input is serialized through the workflow's input codec — the same
/// codec the entry wrapper decodes with — so the replacement run receives
/// exactly the payload shape it expects. On success the engine records
/// `WorkflowContinuedAsNew` and terminates the current run, so an `Ok` value
/// is intentionally uninhabited.
pub fn continue_as_new(
  input: a,
  input_codec: Codec(a),
) -> Result(Nil, error.WorkflowError) {
  case ffi.continue_as_new(input_codec.encode(input)) {
    Ok(value) -> Ok(value)
    Error(raw_error) -> Error(error.WorkflowEngineFailure(message: raw_error))
  }
}
