//// Execution-tier selection: the optional `execution_tier` decorator, its
//// dispatch-config emission, the in-VM wire routing, and the prefixed
//// error-reason round-trip across the child-process boundary.
////
//// The builder/accessor tests assert the SDK contract directly. The routing
//// tests drive `workflow.run` under the test harness: an `InVm` selection
//// crosses the arity-4 `dispatch_activity_in_vm` wire (the shim runs the
//// thunk — the composed runner — exactly like the engine's child process),
//// while absence and remote tiers keep the untouched arity-3 remote wire.

import aion/activity
import aion/codec
import aion/duration
import aion/error
import aion/testing
import aion/workflow
import gleam/dynamic/decode
import gleam/json
import gleam/option.{None, Some}
import gleam/string
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

fn failing_probe(
  name: String,
  runner_error: error.ActivityError,
) -> activity.Activity(Probe, Probe) {
  activity.new(name, Probe(value: "in"), probe_codec(), probe_codec(), fn(_) {
    Error(runner_error)
  })
}

pub fn activity_new_has_no_tier_selection_test() {
  probe_activity("probe")
  |> activity.selected_tier
  |> should.equal(None)
}

pub fn execution_tier_sets_the_selection_test() {
  probe_activity("probe")
  |> activity.execution_tier(activity.InVm)
  |> activity.selected_tier
  |> should.equal(Some(activity.InVm))
}

pub fn execution_tier_last_call_wins_test() {
  probe_activity("probe")
  |> activity.execution_tier(activity.RemoteRust)
  |> activity.execution_tier(activity.InVm)
  |> activity.selected_tier
  |> should.equal(Some(activity.InVm))
}

pub fn tier_is_preserved_through_every_other_decorator_test() {
  let decorated =
    probe_activity("probe")
    |> activity.execution_tier(activity.InVm)
    |> activity.retry(activity.RetryPolicy(
      max_attempts: 3,
      backoff: activity.Fixed(delay: duration.milliseconds(10)),
    ))
    |> activity.timeout(duration.milliseconds(500))
    |> activity.heartbeat(duration.milliseconds(100))
    |> activity.label("brief", "IP-001")
    |> activity.task_queue("gpu")
    |> activity.node("box-7")

  decorated |> activity.selected_tier |> should.equal(Some(activity.InVm))
  // And the tier decorator preserves everything else in return.
  probe_activity("probe")
  |> activity.task_queue("gpu")
  |> activity.execution_tier(activity.InVm)
  |> activity.selected_task_queue
  |> should.equal(Some("gpu"))
}

pub fn no_tier_selection_encodes_null_and_takes_the_remote_wire_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-tierless")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> workflow.run
      |> should.equal(Ok(Probe(value: "in")))

      observed_config(env, "probe-tierless")
      |> field_value("tier")
      |> should.equal(None)
      // The remote wire records the plain `activity:` observation.
      observed(env, "activity:probe-tierless:") |> should.be_true
      observed(env, "activity_in_vm:probe-tierless:") |> should.be_false
    }
    Error(_) -> should.fail()
  }
}

pub fn remote_tier_selection_encodes_but_keeps_the_remote_wire_test() {
  case testing.new() {
    Ok(env) -> {
      let mocked = probe_activity("probe-remote-rust")
      testing.mock_activity(env, mocked, fn(input) { Ok(input) })
      |> should.equal(Ok(env))

      mocked
      |> activity.execution_tier(activity.RemoteRust)
      |> workflow.run
      |> should.equal(Ok(Probe(value: "in")))

      observed_config(env, "probe-remote-rust")
      |> field_value("tier")
      |> should.equal(Some("remote_rust"))
      observed(env, "activity_in_vm:probe-remote-rust:") |> should.be_false
    }
    Error(_) -> should.fail()
  }
}

pub fn in_vm_selection_takes_the_in_vm_wire_and_runs_the_thunk_test() {
  case testing.new() {
    Ok(env) -> {
      // No mock is registered: the shim's in-VM double runs the thunk itself,
      // so a successful result proves the SDK composed the real runner and
      // output codec into the thunk.
      probe_activity("probe-invm")
      |> activity.execution_tier(activity.InVm)
      |> workflow.run
      |> should.equal(Ok(Probe(value: "in")))

      observed_config(env, "probe-invm")
      |> field_value("tier")
      |> should.equal(Some("in_vm"))
      observed(env, "activity_in_vm:probe-invm:") |> should.be_true
    }
    Error(_) -> should.fail()
  }
}

pub fn retryable_error_kind_round_trips_through_the_in_vm_wire_test() {
  case testing.new() {
    Ok(_env) -> {
      failing_probe("probe-invm-retryable", error.retryable("boom"))
      |> activity.execution_tier(activity.InVm)
      |> workflow.run
      |> should.equal(Error(error.Retryable(message: "boom", details: "")))
    }
    Error(_) -> should.fail()
  }
}

pub fn terminal_error_kind_round_trips_through_the_in_vm_wire_test() {
  case testing.new() {
    Ok(_env) -> {
      failing_probe("probe-invm-terminal", error.terminal("no retry"))
      |> activity.execution_tier(activity.InVm)
      |> workflow.run
      |> should.equal(Error(error.Terminal(message: "no retry", details: "")))
    }
    Error(_) -> should.fail()
  }
}

pub fn timeout_and_cancelled_kinds_round_trip_through_the_in_vm_wire_test() {
  case testing.new() {
    Ok(_env) -> {
      failing_probe(
        "probe-invm-timeout",
        error.ActivityTimedOut(error.TimedOut(message: "too slow")),
      )
      |> activity.execution_tier(activity.InVm)
      |> workflow.run
      |> should.equal(
        Error(error.ActivityTimedOut(error.TimedOut(message: "too slow"))),
      )

      failing_probe(
        "probe-invm-cancelled",
        error.ActivityCancelled(error.Cancelled(reason: "operator")),
      )
      |> activity.execution_tier(activity.InVm)
      |> workflow.run
      |> should.equal(
        Error(error.ActivityCancelled(error.Cancelled(reason: "operator"))),
      )
    }
    Error(_) -> should.fail()
  }
}

pub fn engine_failure_crosses_unprefixed_and_round_trips_test() {
  case testing.new() {
    Ok(_env) -> {
      failing_probe(
        "probe-invm-engine",
        error.ActivityEngineFailure(message: "unclassified"),
      )
      |> activity.execution_tier(activity.InVm)
      |> workflow.run
      |> should.equal(
        Error(error.ActivityEngineFailure(message: "unclassified")),
      )
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

/// True when the recorded observation log contains `needle`.
fn observed(env: testing.TestEnv, needle: String) -> Bool {
  case testing.observations(env) {
    Ok(log) -> string.contains(log, needle)
    Error(_) -> False
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
