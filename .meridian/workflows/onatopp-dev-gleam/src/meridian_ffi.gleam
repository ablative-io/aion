/// Native functions provided by the beamr-meridian runtime.

@external(erlang, "meridian_ffi", "run_step_norn")
pub fn run_step_norn(
  name: String,
  profile: String,
  instruction: String,
  schema: String,
) -> Result(String, String)

@external(erlang, "meridian_ffi", "read_file")
pub fn read_file(path: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "write_file")
pub fn write_file(path: String, content: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "run_cmd")
pub fn run_cmd(command: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "commit")
pub fn commit(message: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "parse_json")
pub fn parse_json(json: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "to_json")
pub fn to_json(term: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "set_context")
pub fn set_context(key: String, value: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "get_context")
pub fn get_context(key: String) -> Result(String, String)

// JSON navigation helpers — work with JSON strings throughout.

@external(erlang, "meridian_ffi", "json_get")
pub fn json_get(json: String, key: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "json_string")
pub fn json_string(json: String, key: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "json_bool")
pub fn json_bool(json: String, key: String) -> Result(Bool, String)

@external(erlang, "meridian_ffi", "json_array")
pub fn json_array(json: String, key: String) -> Result(List(String), String)

@external(erlang, "meridian_ffi", "json_string_array")
pub fn json_string_array(json: String, key: String) -> Result(List(String), String)

@external(erlang, "meridian_ffi", "json_opt")
pub fn json_opt(json: String, key: String, default: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "json_opt_array")
pub fn json_opt_array(json: String, key: String) -> List(String)

@external(erlang, "meridian_ffi", "json_unwrap")
pub fn json_unwrap(json: String, key: String) -> Result(String, String)
