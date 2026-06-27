//// NODE-4 node affinity: OPTIONAL per-activity node pin (no workflow default).
////
//// The builder/accessor tests assert the SDK contract directly. The encoding
//// tests drive `workflow.run` under the test harness and read back the dispatch
//// `config` the FFI shim observed, verifying the optional `node` selection
//// crosses the boundary (the engine resolves the affinity). The collect surface
//// shares the same `activity_config` encoder.

import aion/activity
import aion/codec
import aion/testing
import aion/workflow
import gleam/dynamic/decode
import gleam/json
import gleam/option.{None, Some}
import gleeunit/should

/// Test-double accessor: the dispatch `config` JSON the SDK last emitted for an
/// activity name (stored by the in-process FFI shim, no observation side
/// effect). Declared here because it is a test-only channel, not part of the
/// production FFI surface.
@external(erlang, "aion_flow_ffi", "testing_last_activity_config")
fn last_activity_config(name: String) -> Result(String, String)

pub type Probe {
  Probe(value: String)
}

fn probe_codec() -> codec.Codec(Probe) {
  codec.json_codec(
    fn(probe: Probe) { json.object([#("value", json.string(probe.value))]) },
    {
      use value <- decode.field("value", decode.string)
      decode.success(Probe(value: value))
    },
  )
}

fn probe_activity(name: String) -> activity.Activity(Probe, Probe) {
  activity.new(
    name,
    Probe(value: "in"),
    probe_codec(),
    probe_codec(),
    fn(input) { Ok(input) },
  )
}

pub fn activity_new_has_no_node_pin_test() {
  probe_activity("probe")
  |> activity.selected_node
  |> should.equal(None)
}

pub fn activity_node_sets_the_pin_test() {
  probe_activity("probe")
  |> activity.node("box-7")
  |> activity.selected_node
  |> should.equal(Some("box-7"))
}

pub fn activity_node_last_call_wins_test() {
  probe_activity("probe")
  |> activity.node("box-1")
  |> activity.node("box-7")
  |> activity.selected_node
  |> should.equal(Some("box-7"))
}

pub fn explicit_node_is_encoded_in_dispatch_config_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-pinned")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> activity.node("box-7")
      |> workflow.run
      |> should.equal(Ok(Probe(value: "in")))

      let config = observed_config(env, "probe-pinned")
      config |> field_value("node") |> should.equal(Some("box-7"))
    }
    Error(_) -> should.fail()
  }
}

pub fn no_pin_encodes_null_for_node_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-unpinned")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> workflow.run
      |> should.equal(Ok(Probe(value: "in")))

      let config = observed_config(env, "probe-unpinned")
      config |> field_value("node") |> should.equal(None)
    }
    Error(_) -> should.fail()
  }
}

/// Read the dispatch `config` JSON the shim recorded for `activity_name`.
fn observed_config(_env: testing.TestEnv, activity_name: String) -> String {
  case last_activity_config(activity_name) {
    Ok(config) -> config
    Error(_) -> ""
  }
}

/// Decode one optional top-level string field from a `config` JSON object.
/// A JSON `null` (the SDK's "no pin") decodes to `None`.
fn field_value(config: String, field: String) -> option.Option(String) {
  case
    json.parse(config, {
      use value <- decode.optional_field(
        field,
        None,
        decode.optional(decode.string),
      )
      decode.success(value)
    })
  {
    Ok(value) -> value
    Error(_) -> None
  }
}
