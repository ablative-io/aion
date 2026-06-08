//// Minimal Aion workflow used by the getting-started guide.
////
//// The workflow accepts `{ "name": String }`, schedules one remote `greet`
//// activity, and returns the greeting string produced by the worker.

pub type WorkflowError {
  ActivityFailed(message: String)
}

@external(erlang, "aion_flow_ffi", "run_activity")
fn raw_nif(name: String, input: String, config: String) -> Result(String, String)

pub fn run(input: String) -> Result(String, WorkflowError) {
  case raw_nif("greet", input, "{}") {
    Ok(result) -> Ok(result)
    Error(err) -> Error(ActivityFailed(err))
  }
}
