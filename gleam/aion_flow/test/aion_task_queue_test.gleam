//// NSTQ-4 task-queue selection: per-activity override + workflow-level default.
////
//// The builder/accessor tests assert the SDK contract directly. The encoding
//// tests drive `workflow.run`/`run_with_default` under the test harness and
//// read back the dispatch `config` the FFI shim observed, verifying both
//// task-queue selections cross the boundary unresolved (the engine applies the
//// precedence). The collect surface shares the same `activity_config` encoder.

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

pub fn activity_new_has_no_task_queue_selection_test() {
  probe_activity("probe")
  |> activity.selected_task_queue
  |> should.equal(None)
}

pub fn activity_task_queue_sets_the_override_test() {
  probe_activity("probe")
  |> activity.task_queue("claude")
  |> activity.selected_task_queue
  |> should.equal(Some("claude"))
}

pub fn activity_task_queue_last_call_wins_test() {
  probe_activity("probe")
  |> activity.task_queue("cpu")
  |> activity.task_queue("gpu")
  |> activity.selected_task_queue
  |> should.equal(Some("gpu"))
}

pub fn explicit_task_queue_is_encoded_in_dispatch_config_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-override")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> activity.task_queue("claude")
      |> workflow.run
      |> should.equal(Ok(Probe(value: "in")))

      let config = observed_config(env, "probe-override")
      config |> field_value("task_queue") |> should.equal(Some("claude"))
      config |> field_value("workflow_task_queue") |> should.equal(None)
    }
    Error(_) -> should.fail()
  }
}

pub fn no_selection_encodes_null_for_both_fields_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-none")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> workflow.run
      |> should.equal(Ok(Probe(value: "in")))

      let config = observed_config(env, "probe-none")
      config |> field_value("task_queue") |> should.equal(None)
      config |> field_value("workflow_task_queue") |> should.equal(None)
    }
    Error(_) -> should.fail()
  }
}

pub fn workflow_default_is_encoded_when_activity_selects_none_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-default")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> workflow.run_with_default(Some("gpu"))
      |> should.equal(Ok(Probe(value: "in")))

      let config = observed_config(env, "probe-default")
      // No activity override; the workflow default crosses for the engine to
      // resolve (the SDK does NOT collapse it into `task_queue`).
      config |> field_value("task_queue") |> should.equal(None)
      config |> field_value("workflow_task_queue") |> should.equal(Some("gpu"))
    }
    Error(_) -> should.fail()
  }
}

pub fn override_and_workflow_default_both_cross_unresolved_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-both")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> activity.task_queue("claude")
      |> workflow.run_with_default(Some("gpu"))
      |> should.equal(Ok(Probe(value: "in")))

      let config = observed_config(env, "probe-both")
      // Both selections present and unresolved: the engine seam picks "claude".
      config |> field_value("task_queue") |> should.equal(Some("claude"))
      config |> field_value("workflow_task_queue") |> should.equal(Some("gpu"))
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
/// A JSON `null` (the SDK's "no selection") decodes to `None`.
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
