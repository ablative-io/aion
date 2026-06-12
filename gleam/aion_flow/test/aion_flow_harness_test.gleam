//// aion/testing harness tests.

import aion/activity
import aion/codec
import aion/duration
import aion/error
import aion/testing
import aion/testing/replay
import aion/workflow
import gleam/dynamic/decode
import gleam/json
import gleeunit
import gleeunit/should

pub fn main() {
  gleeunit.main()
}

pub type ChargeRequest {
  ChargeRequest(order_id: String, cents: Int)
}

pub type ChargeReceipt {
  ChargeReceipt(id: String, approved: Bool)
}

pub fn trivial_workflow_runs_under_test_env_test() {
  case testing.run(fn(_env) { Ok("typed-result") }) {
    Ok(result) -> result |> should.equal(Ok("typed-result"))
    Error(_) -> should.fail()
  }
}

pub fn simulated_now_reflects_advanced_time_test() {
  case testing.new() {
    Ok(env) -> {
      case workflow.now() {
        Ok(before) ->
          before
          |> workflow.timestamp_to_milliseconds
          |> should.equal(1_700_000_000_000)
        Error(_) -> should.fail()
      }

      testing.advance(env, duration.seconds(90))
      |> should.equal(Ok(env))

      case workflow.now() {
        Ok(after) ->
          after
          |> workflow.timestamp_to_milliseconds
          |> should.equal(1_700_000_090_000)
        Error(_) -> should.fail()
      }

      testing.new()
      |> should.equal(Ok(env))
    }
    Error(_) -> should.fail()
  }
}

pub fn sleep_for_days_completes_without_wall_clock_wait_test() {
  case testing.new() {
    Ok(env) -> {
      workflow.sleep(duration.days(30))
      |> should.equal(Ok(Nil))

      testing.advance(env, duration.days(30))
      |> should.equal(Ok(env))
    }
    Error(_) -> should.fail()
  }
}

pub fn activity_mock_returns_canned_typed_result_test() {
  case testing.new() {
    Ok(env) -> {
      let request = ChargeRequest(order_id: "order-100", cents: 1200)
      let activity_value =
        charge_activity("mock-charge", request, fn(_request) {
          error.terminal("unmocked")
          |> Error
        })

      testing.mock_activity(env, activity_value, fn(typed_request) {
        Ok(ChargeReceipt(
          id: "mock-receipt-" <> typed_request.order_id,
          approved: True,
        ))
      })
      |> should.equal(Ok(env))

      workflow.run(activity_value)
      |> should.equal(
        Ok(ChargeReceipt(id: "mock-receipt-order-100", approved: True)),
      )
    }
    Error(_) -> should.fail()
  }
}

pub fn activity_mock_returns_retryable_failure_test() {
  case testing.new() {
    Ok(env) -> {
      let activity_value =
        charge_activity(
          "mock-retry",
          ChargeRequest(order_id: "order-101", cents: 900),
          fn(_request) { Ok(ChargeReceipt(id: "unmocked", approved: False)) },
        )

      testing.mock_activity(env, activity_value, fn(_request) {
        Error(error.retryable("please retry"))
      })
      |> should.equal(Ok(env))

      workflow.run(activity_value)
      |> should.equal(
        Error(error.Retryable(message: "please retry", details: "")),
      )
    }
    Error(_) -> should.fail()
  }
}

pub fn deterministic_replay_assertion_passes_test() {
  case testing.new() {
    Ok(env) -> {
      let activity_value =
        charge_activity(
          "replay-charge",
          ChargeRequest(order_id: "order-102", cents: 500),
          fn(_request) { Ok(ChargeReceipt(id: "unmocked", approved: False)) },
        )

      testing.mock_activity(env, activity_value, fn(typed_request) {
        Ok(ChargeReceipt(
          id: "replay-" <> typed_request.order_id,
          approved: True,
        ))
      })
      |> should.equal(Ok(env))

      testing.assert_replay(env, fn() { workflow.run(activity_value) })
      |> should.equal(Ok(ChargeReceipt(id: "replay-order-102", approved: True)))
    }
    Error(_) -> should.fail()
  }
}

pub fn non_deterministic_replay_assertion_fails_with_diagnostic_test() {
  case testing.new() {
    Ok(env) -> {
      let first =
        charge_activity(
          "branch-a",
          ChargeRequest(order_id: "order-103", cents: 500),
          fn(_request) { Ok(ChargeReceipt(id: "unmocked", approved: False)) },
        )
      let second =
        charge_activity(
          "branch-b",
          ChargeRequest(order_id: "order-103", cents: 500),
          fn(_request) { Ok(ChargeReceipt(id: "unmocked", approved: False)) },
        )

      testing.mock_activity(env, first, fn(typed_request) {
        Ok(ChargeReceipt(id: "a-" <> typed_request.order_id, approved: True))
      })
      |> should.equal(Ok(env))
      testing.mock_activity(env, second, fn(typed_request) {
        Ok(ChargeReceipt(id: "b-" <> typed_request.order_id, approved: True))
      })
      |> should.equal(Ok(env))

      case
        testing.assert_replay(env, fn() {
          case workflow.now() {
            Ok(timestamp) -> {
              let chosen = case workflow.timestamp_to_milliseconds(timestamp) {
                1_700_000_000_000 -> first
                _ -> second
              }
              case workflow.run(chosen) {
                Ok(receipt) -> {
                  case testing.advance(env, duration.milliseconds(1)) {
                    Ok(_) -> Ok(receipt)
                    Error(_) ->
                      Error(error.ActivityEngineFailure("advance failed"))
                  }
                }
                Error(activity_error) -> Error(activity_error)
              }
            }
            Error(error.EngineFailure(message: message)) ->
              Error(error.ActivityEngineFailure("now failed: " <> message))
          }
        })
      {
        Error(replay.ObservationMismatch(recorded: recorded, replayed: replayed)) -> {
          should.be_true(recorded != replayed)
          testing.new()
          |> should.equal(Ok(env))
        }
        _ -> should.fail()
      }
    }
    Error(_) -> should.fail()
  }
}

pub type RefusalReason {
  RefusalReason(reason: String)
}

pub fn child_mock_runs_typed_handler_and_returns_output_test() {
  case testing.new() {
    Ok(env) -> {
      testing.mock_child(
        env,
        "typed-child",
        charge_request_codec(),
        charge_receipt_codec(),
        refusal_codec(),
        fn(request: ChargeRequest) {
          Ok(ChargeReceipt(id: "child-" <> request.order_id, approved: True))
        },
      )
      |> should.equal(Ok(env))

      workflow.spawn_and_wait(
        "typed-child",
        fn(_request: ChargeRequest) {
          Error(RefusalReason(reason: "unreached type anchor"))
        },
        ChargeRequest(order_id: "order-200", cents: 700),
        charge_request_codec(),
        charge_receipt_codec(),
        refusal_codec(),
      )
      |> should.equal(Ok(ChargeReceipt(id: "child-order-200", approved: True)))
    }
    Error(_) -> should.fail()
  }
}

pub fn child_mock_typed_error_surfaces_as_child_workflow_failed_test() {
  case testing.new() {
    Ok(env) -> {
      testing.mock_child(
        env,
        "refusing-child",
        charge_request_codec(),
        charge_receipt_codec(),
        refusal_codec(),
        fn(_request: ChargeRequest) {
          Error(RefusalReason(reason: "limit exceeded"))
        },
      )
      |> should.equal(Ok(env))

      workflow.spawn_and_wait(
        "refusing-child",
        fn(_request: ChargeRequest) {
          Error(RefusalReason(reason: "unreached type anchor"))
        },
        ChargeRequest(order_id: "order-201", cents: 900),
        charge_request_codec(),
        charge_receipt_codec(),
        refusal_codec(),
      )
      |> should.equal(
        Error(
          error.ChildWorkflowFailed(RefusalReason(reason: "limit exceeded")),
        ),
      )
    }
    Error(_) -> should.fail()
  }
}

pub fn unregistered_child_name_keeps_legacy_fixture_behaviour_test() {
  case testing.new() {
    Ok(env) -> {
      // No child mock registered: the FFI double's legacy fixtures answer,
      // and unknown names stay loud engine failures.
      case
        workflow.spawn_and_wait(
          "never-registered-child",
          fn(_request: ChargeRequest) {
            Error(RefusalReason(reason: "unreached type anchor"))
          },
          ChargeRequest(order_id: "order-202", cents: 100),
          charge_request_codec(),
          charge_receipt_codec(),
          refusal_codec(),
        )
      {
        Error(error.ChildEngineFailure(message: _)) ->
          testing.new()
          |> should.equal(Ok(env))
        _ -> should.fail()
      }
    }
    Error(_) -> should.fail()
  }
}

fn refusal_codec() -> codec.Codec(RefusalReason) {
  codec.json_codec(
    fn(refusal: RefusalReason) {
      json.object([#("reason", json.string(refusal.reason))])
    },
    {
      use reason <- decode.field("reason", decode.string)
      decode.success(RefusalReason(reason: reason))
    },
  )
}

fn charge_activity(
  name: String,
  request: ChargeRequest,
  runner: fn(ChargeRequest) -> Result(ChargeReceipt, error.ActivityError),
) -> activity.Activity(ChargeRequest, ChargeReceipt) {
  activity.new(
    name,
    request,
    charge_request_codec(),
    charge_receipt_codec(),
    runner,
  )
}

fn charge_request_codec() -> codec.Codec(ChargeRequest) {
  codec.json_codec(charge_request_to_json, charge_request_decoder())
}

fn charge_request_to_json(request: ChargeRequest) -> json.Json {
  json.object([
    #("order_id", json.string(request.order_id)),
    #("cents", json.int(request.cents)),
  ])
}

fn charge_request_decoder() -> decode.Decoder(ChargeRequest) {
  use order_id <- decode.field("order_id", decode.string)
  use cents <- decode.field("cents", decode.int)
  decode.success(ChargeRequest(order_id: order_id, cents: cents))
}

fn charge_receipt_codec() -> codec.Codec(ChargeReceipt) {
  codec.json_codec(charge_receipt_to_json, charge_receipt_decoder())
}

fn charge_receipt_to_json(receipt: ChargeReceipt) -> json.Json {
  json.object([
    #("id", json.string(receipt.id)),
    #("approved", json.bool(receipt.approved)),
  ])
}

fn charge_receipt_decoder() -> decode.Decoder(ChargeReceipt) {
  use id <- decode.field("id", decode.string)
  use approved <- decode.field("approved", decode.bool)
  decode.success(ChargeReceipt(id: id, approved: approved))
}
