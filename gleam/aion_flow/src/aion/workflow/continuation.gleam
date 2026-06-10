//// continue_as_new workflow primitive.

import aion/error
import aion/internal/ffi

@external(erlang, "aion_flow_continue", "encode")
fn encode(input: a) -> String

/// Continue the current workflow as a new run with fresh history.
///
/// The input is serialized before crossing the engine NIF boundary. On success
/// the engine records `WorkflowContinuedAsNew` and terminates the current run, so
/// an `Ok` value is intentionally uninhabited.
pub fn continue_as_new(input: a) -> Result(Nil, error.WorkflowError) {
  let encoded_input = encode(input)
  case ffi.continue_as_new(encoded_input) {
    Ok(value) -> Ok(value)
    Error(raw_error) -> Error(error.WorkflowEngineFailure(message: raw_error))
  }
}
