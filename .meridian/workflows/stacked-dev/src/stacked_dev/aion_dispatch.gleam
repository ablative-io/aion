//// Bindings to Aion's two-phase activity NIFs for Meridian worker dispatch.

@external(erlang, "aion_flow_ffi", "dispatch_activity")
pub fn dispatch_activity(
  name: String,
  input_json: String,
  config_json: String,
) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "await_activity_result")
pub fn await_activity_result(correlation_id: String) -> Result(String, String)
