//// aion_flow foundational primitive tests.

import aion/activity
import aion/child
import aion/codec
import aion/duration
import aion/error
import aion/query
import aion/signal
import aion/testing
import aion/workflow
import gleam/dynamic/decode
import gleam/json
import gleam/option.{None, Some}
import gleam/result
import gleeunit
import gleeunit/should

pub fn main() {
  gleeunit.main()
}

pub type LineItem {
  LineItem(sku: String, quantity: Int)
}

pub type Order {
  Order(id: String, items: List(LineItem))
}

pub type ChargeRequest {
  ChargeRequest(order_id: String, cents: Int)
}

pub type ChargeReceipt {
  ChargeReceipt(id: String, approved: Bool)
}

pub type ExampleOrder {
  ExampleOrder(id: String, cents: Int)
}

pub type ExampleDecision {
  ExampleDecision(approved: Bool)
}

pub type ExampleSummary {
  ExampleSummary(
    order_id: String,
    run_receipt_id: String,
    child_receipt_id: String,
    fanout_receipt_ids: List(String),
    approved: Bool,
    observed_at_milliseconds: Int,
  )
}

pub fn codec_round_trips_record_test() {
  let order_codec = codec.json_codec(order_to_json, order_decoder())
  let order = Order(id: "order-1", items: [LineItem(sku: "sku-1", quantity: 2)])

  order_codec.decode(order_codec.encode(order))
  |> should.equal(Ok(order))
}

pub fn codec_round_trips_list_test() {
  let numbers_codec =
    codec.json_codec(int_list_to_json, decode.list(decode.int))
  let numbers = [1, 2, 3, 5, 8]

  numbers_codec.decode(numbers_codec.encode(numbers))
  |> should.equal(Ok(numbers))
}

pub fn codec_round_trips_nested_value_test() {
  let order_codec = codec.json_codec(order_to_json, order_decoder())
  let order =
    Order(id: "order-2", items: [
      LineItem(sku: "sku-2", quantity: 1),
      LineItem(sku: "sku-3", quantity: 4),
    ])

  order_codec.decode(order_codec.encode(order))
  |> should.equal(Ok(order))
}

pub fn codec_malformed_json_returns_decode_error_test() {
  let order_codec = codec.json_codec(order_to_json, order_decoder())

  order_codec.decode("{")
  |> should.equal(
    Error(codec.DecodeError(reason: "Unexpected end of input", path: [])),
  )
}

pub fn codec_schema_error_carries_path_test() {
  let order_codec = codec.json_codec(order_to_json, order_decoder())
  let malformed =
    "{\"id\":\"order-3\",\"items\":[{\"sku\":\"sku-4\",\"quantity\":\"many\"}]}"

  case order_codec.decode(malformed) {
    Ok(_) -> should.fail()
    Error(codec.DecodeError(reason: reason, path: path)) -> {
      reason |> should.equal("Expected Int, found String")
      path |> should.equal(["items", "0", "quantity"])
    }
  }
}

pub fn activity_error_constructors_preserve_classification_test() {
  let retryable =
    error.retryable_with_details("network failed", "{\"attempt\":1}")
  let terminal =
    error.terminal_with_details("invalid order", "{\"field\":\"id\"}")

  case retryable {
    error.Retryable(message: message, details: details) -> {
      message |> should.equal("network failed")
      details |> should.equal("{\"attempt\":1}")
    }
    _ -> should.fail()
  }

  case terminal {
    error.Terminal(message: message, details: details) -> {
      message |> should.equal("invalid order")
      details |> should.equal("{\"field\":\"id\"}")
    }
    _ -> should.fail()
  }
}

pub fn activity_error_helpers_use_empty_details_test() {
  case error.retryable("temporary outage") {
    error.Retryable(message: message, details: details) -> {
      message |> should.equal("temporary outage")
      details |> should.equal("")
    }
    _ -> should.fail()
  }

  case error.terminal("bad request") {
    error.Terminal(message: message, details: details) -> {
      message |> should.equal("bad request")
      details |> should.equal("")
    }
    _ -> should.fail()
  }
}

pub fn duration_equivalent_minutes_and_seconds_are_canonical_test() {
  duration.minutes(1)
  |> should.equal(duration.seconds(60))

  duration.minutes(1)
  |> duration.to_milliseconds
  |> should.equal(60_000)

  duration.seconds(60)
  |> duration.to_milliseconds
  |> should.equal(60_000)
}

pub fn duration_equivalent_days_and_hours_are_canonical_test() {
  duration.days(1)
  |> should.equal(duration.hours(24))

  duration.days(1)
  |> duration.to_milliseconds
  |> should.equal(86_400_000)

  duration.milliseconds(86_400_000)
  |> should.equal(duration.days(1))
}

pub fn activity_new_carries_typed_fields_test() {
  let request = ChargeRequest(order_id: "order-activity-1", cents: 4200)
  let request_codec = charge_request_codec()
  let receipt_codec = charge_receipt_codec()
  let charge =
    activity.new(
      "charge-payment",
      request,
      request_codec,
      receipt_codec,
      charge_payment,
    )

  charge
  |> activity.name
  |> should.equal("charge-payment")

  charge
  |> activity.input
  |> should.equal(request)

  let input_codec = activity.input_codec(charge)
  input_codec.encode(request)
  |> should.equal("{\"order_id\":\"order-activity-1\",\"cents\":4200}")

  let codec = activity.output_codec(charge)
  codec.encode(ChargeReceipt(id: "receipt-1", approved: True))
  |> should.equal("{\"id\":\"receipt-1\",\"approved\":true}")

  let run = activity.runner(charge)
  run(request)
  |> should.equal(
    Ok(ChargeReceipt(id: "receipt-order-activity-1", approved: True)),
  )
}

pub fn activity_decorators_compose_and_carry_config_test() {
  let policy =
    activity.RetryPolicy(
      max_attempts: 4,
      backoff: activity.Fixed(delay: duration.seconds(2)),
    )
  let configured =
    activity.new(
      "charge-payment",
      ChargeRequest(order_id: "order-activity-2", cents: 1200),
      charge_request_codec(),
      charge_receipt_codec(),
      charge_payment,
    )
    |> activity.retry(policy)
    |> activity.timeout(duration.seconds(30))
    |> activity.heartbeat(duration.seconds(5))

  configured
  |> activity.retry_policy
  |> should.equal(Some(policy))

  configured
  |> activity.timeout_duration
  |> should.equal(Some(duration.seconds(30)))

  configured
  |> activity.heartbeat_interval
  |> should.equal(Some(duration.seconds(5)))
}

pub fn activity_backoff_variants_and_retry_policy_carry_values_test() {
  let exponential =
    activity.Exponential(
      initial: duration.seconds(1),
      multiplier: 2.0,
      max: duration.minutes(1),
    )
  let linear =
    activity.Linear(
      initial: duration.seconds(1),
      increment: duration.seconds(3),
      max: duration.seconds(30),
    )
  let fixed = activity.Fixed(delay: duration.seconds(5))
  let policy = activity.RetryPolicy(max_attempts: 6, backoff: exponential)

  exponential
  |> should.equal(activity.Exponential(
    initial: duration.seconds(1),
    multiplier: 2.0,
    max: duration.seconds(60),
  ))

  linear
  |> should.equal(activity.Linear(
    initial: duration.milliseconds(1000),
    increment: duration.seconds(3),
    max: duration.seconds(30),
  ))

  fixed
  |> should.equal(activity.Fixed(delay: duration.milliseconds(5000)))

  policy
  |> should.equal(activity.RetryPolicy(max_attempts: 6, backoff: exponential))
}

pub fn activity_new_has_no_default_policy_or_timing_config_test() {
  let charge =
    activity.new(
      "charge-payment",
      ChargeRequest(order_id: "order-activity-3", cents: 900),
      charge_request_codec(),
      charge_receipt_codec(),
      charge_payment,
    )

  charge
  |> activity.retry_policy
  |> should.equal(None)

  charge
  |> activity.timeout_duration
  |> should.equal(None)

  charge
  |> activity.heartbeat_interval
  |> should.equal(None)
}

pub fn workflow_run_returns_decoded_typed_result_test() {
  let charge =
    activity.new(
      "charge-payment",
      ChargeRequest(order_id: "order-run-1", cents: 700),
      charge_request_codec(),
      charge_receipt_codec(),
      charge_payment,
    )

  charge
  |> workflow.run
  |> should.equal(Ok(ChargeReceipt(id: "receipt-order-run-1", approved: True)))
}

pub fn workflow_run_returns_typed_activity_error_test() {
  let failing =
    activity.new(
      "fail-retryable",
      ChargeRequest(order_id: "order-run-2", cents: 700),
      charge_request_codec(),
      charge_receipt_codec(),
      charge_payment,
    )

  failing
  |> workflow.run
  |> should.equal(Error(error.Retryable(message: "mock retry", details: "")))
}

pub fn workflow_all_returns_ordered_typed_results_test() {
  let activities = [
    charge_activity("charge-payment", "order-all-1"),
    charge_activity("charge-payment", "order-all-2"),
    charge_activity("charge-payment", "order-all-3"),
  ]

  workflow.all(activities)
  |> should.equal(
    Ok([
      ChargeReceipt(id: "receipt-order-all-1", approved: True),
      ChargeReceipt(id: "receipt-order-all-2", approved: True),
      ChargeReceipt(id: "receipt-order-all-3", approved: True),
    ]),
  )
}

pub fn workflow_all_single_failure_fails_collection_test() {
  let activities = [
    charge_activity("charge-payment", "order-all-ok"),
    charge_activity("fail-retryable", "order-all-fail"),
    charge_activity("charge-payment", "order-all-cancelled"),
  ]

  workflow.all(activities)
  |> should.equal(Error(error.Retryable(message: "mock retry", details: "")))
}

pub fn workflow_race_first_success_wins_and_loser_is_cancelled_test() {
  let before = query.recorded_observations()
  let activities = [
    charge_activity("slow-charge-payment", "order-race-slow"),
    charge_activity("charge-payment", "order-race-fast"),
  ]

  workflow.race(activities)
  |> should.equal(
    Ok(ChargeReceipt(id: "receipt-order-race-fast", approved: True)),
  )

  query.recorded_observations()
  |> should.equal(increment_count(before))
}

pub fn workflow_race_first_failure_wins_test() {
  let before = query.recorded_observations()
  let activities = [
    charge_activity("race-fail-fast", "order-race-fail"),
    charge_activity("charge-payment", "order-race-loser"),
  ]

  workflow.race(activities)
  |> should.equal(
    Error(error.Terminal(message: "race failed first", details: "")),
  )

  query.recorded_observations()
  |> should.equal(increment_count(before))
}

pub fn workflow_map_dynamic_fanout_returns_ordered_results_test() {
  ["order-map-1", "order-map-2", "order-map-3"]
  |> workflow.map(fn(order_id) { charge_activity("charge-payment", order_id) })
  |> should.equal(
    Ok([
      ChargeReceipt(id: "receipt-order-map-1", approved: True),
      ChargeReceipt(id: "receipt-order-map-2", approved: True),
      ChargeReceipt(id: "receipt-order-map-3", approved: True),
    ]),
  )
}

pub fn workflow_map_empty_returns_empty_result_test() {
  []
  |> workflow.map(fn(order_id) { charge_activity("charge-payment", order_id) })
  |> should.equal(Ok([]))
}

pub fn workflow_run_decode_failure_is_typed_data_test() {
  let malformed =
    activity.new(
      "malformed-receipt",
      ChargeRequest(order_id: "order-run-3", cents: 700),
      charge_request_codec(),
      charge_receipt_codec(),
      charge_payment,
    )

  case workflow.run(malformed) {
    Ok(_) -> should.fail()
    Error(error.ActivityDecodeFailed(decode_error)) ->
      decode_error
      |> should.equal(
        codec.DecodeError(reason: "Expected String, found Int", path: ["id"]),
      )
    Error(_) -> should.fail()
  }
}

pub fn workflow_now_and_random_are_deterministic_bindings_test() {
  case workflow.now() {
    Ok(timestamp) ->
      timestamp
      |> workflow.timestamp_to_milliseconds
      |> should.equal(1_700_000_000_000)
    Error(_) -> should.fail()
  }

  workflow.random()
  |> should.equal(Ok(0.25))

  workflow.random_int(1, 10)
  |> should.equal(Ok(4))
}

pub fn workflow_random_int_rejects_invalid_range_before_dispatch_test() {
  workflow.random_int(10, 1)
  |> should.equal(
    Error(error.EngineFailure(
      message: "Invalid deterministic random_int range: min is greater than max",
    )),
  )
}

pub fn signal_new_carries_typed_name_and_codec_test() {
  let payment_signal = signal.new("payment-authorized", charge_request_codec())

  payment_signal
  |> signal.name
  |> should.equal("payment-authorized")

  signal.codec(payment_signal).encode(ChargeRequest(
    order_id: "order-signal-ref",
    cents: 5100,
  ))
  |> should.equal("{\"order_id\":\"order-signal-ref\",\"cents\":5100}")
}

pub fn signal_send_and_workflow_receive_return_typed_payload_test() {
  let payment_signal = signal.new("payment-received", charge_request_codec())
  let payload = ChargeRequest(order_id: "order-signal-receive", cents: 3300)

  signal.send("workflow-1", payment_signal, payload)
  |> should.equal(Ok(Nil))

  workflow.receive(payment_signal)
  |> should.equal(Ok(payload))
}

pub fn workflow_receive_decode_failure_is_typed_data_test() {
  let payment_signal = signal.new("malformed-signal", charge_request_codec())

  case workflow.receive(payment_signal) {
    Ok(_) -> should.fail()
    Error(error.ReceiveDecodeFailed(decode_error)) ->
      decode_error
      |> should.equal(
        codec.DecodeError(reason: "Expected String, found Int", path: [
          "order_id",
        ]),
      )
    Error(_) -> should.fail()
  }
}

pub fn workflow_receive_composes_with_timeout_test() {
  let payment_signal =
    signal.new("payment-with-timeout", charge_request_codec())
  let payload = ChargeRequest(order_id: "order-signal-timeout", cents: 4400)

  signal.send("workflow-1", payment_signal, payload)
  |> should.equal(Ok(Nil))

  workflow.with_timeout(
    fn() { workflow.receive(payment_signal) },
    duration.seconds(1),
  )
  |> should.equal(Ok(payload))
}

pub fn query_handler_returns_typed_decoded_value_test() {
  let state = ChargeReceipt(id: "receipt-query", approved: True)

  query.handler("checkout-state", charge_receipt_codec(), fn() { state })
  |> should.equal(Ok(Nil))

  query.dispatch("checkout-state", charge_receipt_codec())
  |> should.equal(Ok(state))
}

pub fn query_unknown_name_returns_typed_error_test() {
  query.dispatch("missing-query", charge_receipt_codec())
  |> should.equal(Error(error.UnknownQuery("missing-query")))
}

pub fn query_decode_failure_is_typed_data_test() {
  query.handler("malformed-query-reply", charge_receipt_codec(), fn() {
    ChargeReceipt(id: "receipt-query", approved: True)
  })
  |> should.equal(Ok(Nil))

  case query.dispatch("malformed-query-reply", charge_request_codec()) {
    Ok(_) -> should.fail()
    Error(error.QueryDecodeFailed(decode_error)) ->
      decode_error
      |> should.equal(
        codec.DecodeError(reason: "Expected Field, found Nothing", path: [
          "order_id",
        ]),
      )
    Error(_) -> should.fail()
  }
}

pub fn query_dispatch_records_no_observation_test() {
  let state = ChargeReceipt(id: "receipt-no-event", approved: True)
  let before = query.recorded_observations()

  query.handler("no-event-state", charge_receipt_codec(), fn() { state })
  |> should.equal(Ok(Nil))

  query.dispatch("no-event-state", charge_receipt_codec())
  |> should.equal(Ok(state))

  query.recorded_observations()
  |> should.equal(before)
}

pub fn workflow_spawn_returns_typed_child_handle_and_records_started_test() {
  let before = query.recorded_observations()
  let request = ChargeRequest(order_id: "order-child-spawn", cents: 2500)

  case
    workflow.spawn(
      "checkout-child",
      checkout_workflow,
      request,
      charge_request_codec(),
      charge_receipt_codec(),
      workflow_error_codec(),
    )
  {
    Ok(handle) -> {
      handle
      |> child.child_id
      |> should.equal("1")

      let typed_handle: workflow.ChildHandle(ChargeReceipt, String) = handle
      child.output_codec(typed_handle).encode(ChargeReceipt(
        id: "typed-child",
        approved: True,
      ))
      |> should.equal("{\"id\":\"typed-child\",\"approved\":true}")
    }
    Error(_) -> should.fail()
  }

  query.recorded_observations()
  |> should.equal(increment_count(before))
}

pub fn child_await_decodes_completed_child_result_test() {
  let request = ChargeRequest(order_id: "order-child-await", cents: 3100)

  case
    workflow.spawn(
      "checkout-child",
      checkout_workflow,
      request,
      charge_request_codec(),
      charge_receipt_codec(),
      workflow_error_codec(),
    )
  {
    Ok(handle) ->
      child.await(handle)
      |> should.equal(
        Ok(ChargeReceipt(id: "child-receipt-order-child-await", approved: True)),
      )
    Error(_) -> should.fail()
  }
}

pub fn workflow_spawn_and_wait_returns_decoded_ok_test() {
  let request = ChargeRequest(order_id: "order-child-wait", cents: 4100)

  workflow.spawn_and_wait(
    "checkout-child",
    checkout_workflow,
    request,
    charge_request_codec(),
    charge_receipt_codec(),
    workflow_error_codec(),
  )
  |> should.equal(
    Ok(ChargeReceipt(id: "child-receipt-order-child-wait", approved: True)),
  )
}

pub fn workflow_spawn_and_wait_returns_decoded_child_error_test() {
  let request = ChargeRequest(order_id: "order-child-fail", cents: 5100)

  workflow.spawn_and_wait(
    "declining-child",
    checkout_workflow,
    request,
    charge_request_codec(),
    charge_receipt_codec(),
    workflow_error_codec(),
  )
  |> should.equal(Error(error.ChildWorkflowFailed("declined")))
}

pub fn child_await_decode_failure_is_typed_data_test() {
  let request = ChargeRequest(order_id: "order-child-malformed", cents: 6100)

  case
    workflow.spawn(
      "malformed-child",
      checkout_workflow,
      request,
      charge_request_codec(),
      charge_receipt_codec(),
      workflow_error_codec(),
    )
  {
    Ok(handle) ->
      case child.await(handle) {
        Ok(_) -> should.fail()
        Error(error.ChildOutputDecodeFailed(decode_error)) ->
          decode_error
          |> should.equal(
            codec.DecodeError(reason: "Expected String, found Int", path: ["id"]),
          )
        Error(_) -> should.fail()
      }
    Error(_) -> should.fail()
  }
}

pub fn workflow_define_carries_entry_contract_test() {
  let definition =
    workflow.define(
      "checkout",
      charge_request_codec(),
      charge_receipt_codec(),
      workflow_error_codec(),
      checkout_workflow,
    )

  definition
  |> workflow.name
  |> should.equal("checkout")

  workflow.input_codec(definition).encode(ChargeRequest(
    order_id: "order-define",
    cents: 1000,
  ))
  |> should.equal("{\"order_id\":\"order-define\",\"cents\":1000}")

  workflow.output_codec(definition).decode(
    "{\"id\":\"receipt-define\",\"approved\":true}",
  )
  |> should.equal(Ok(ChargeReceipt(id: "receipt-define", approved: True)))

  workflow.error_codec(definition).decode("\"declined\"")
  |> should.equal(Ok("declined"))

  let entry = workflow.entry_fn(definition)
  entry(ChargeRequest(order_id: "order-entry", cents: 1200))
  |> should.equal(Ok(ChargeReceipt(id: "receipt-order-entry", approved: True)))
}

pub fn canonical_example_workflow_runs_under_harness_test() {
  case testing.new() {
    Ok(env) -> {
      let run_activity =
        example_charge_activity("example-charge", "order-example", 2400)
      let fanout_activity =
        example_charge_activity("example-fanout", "order-example-a", 2400)
      let approval_signal = example_decision_signal()
      let definition =
        workflow.define(
          "canonical-example",
          example_order_codec(),
          example_summary_codec(),
          workflow_error_codec(),
          canonical_example_workflow,
        )

      testing.mock_activity(env, run_activity, fn(request) {
        Ok(ChargeReceipt(id: "run-" <> request.order_id, approved: True))
      })
      |> should.equal(Ok(env))

      testing.mock_activity(env, fanout_activity, fn(request) {
        Ok(ChargeReceipt(id: "fanout-" <> request.order_id, approved: True))
      })
      |> should.equal(Ok(env))

      signal.send(
        "workflow-canonical-example",
        approval_signal,
        ExampleDecision(approved: True),
      )
      |> should.equal(Ok(Nil))

      let entry = workflow.entry_fn(definition)
      let result = entry(ExampleOrder(id: "order-example", cents: 2400))

      testing.advance(env, duration.minutes(5))
      |> should.equal(Ok(env))

      result
      |> should.equal(
        Ok(ExampleSummary(
          order_id: "order-example",
          run_receipt_id: "run-order-example",
          child_receipt_id: "child-receipt-order-example",
          fanout_receipt_ids: [
            "fanout-order-example-a",
            "fanout-order-example-b",
          ],
          approved: True,
          observed_at_milliseconds: 1_700_000_000_000,
        )),
      )
    }
    Error(_) -> should.fail()
  }
}

fn increment_count(
  count: Result(Int, error.QueryError),
) -> Result(Int, error.QueryError) {
  case count {
    Ok(value) -> Ok(value + 1)
    Error(error) -> Error(error)
  }
}

fn order_to_json(order: Order) -> json.Json {
  json.object([
    #("id", json.string(order.id)),
    #("items", json.array(order.items, line_item_to_json)),
  ])
}

fn line_item_to_json(item: LineItem) -> json.Json {
  json.object([
    #("sku", json.string(item.sku)),
    #("quantity", json.int(item.quantity)),
  ])
}

fn int_list_to_json(values: List(Int)) -> json.Json {
  json.array(values, json.int)
}

fn charge_payment(
  request: ChargeRequest,
) -> Result(ChargeReceipt, error.ActivityError) {
  Ok(ChargeReceipt(id: "receipt-" <> request.order_id, approved: True))
}

fn canonical_example_workflow(
  input: ExampleOrder,
) -> Result(ExampleSummary, String) {
  use observed_at <- result.try(workflow.now() |> result_map_engine_error)
  use run_receipt <-
    result.try(
      workflow.run(example_charge_activity(
        "example-charge",
        input.id,
        input.cents,
      ))
      |> result_map_activity_error,
    )
  use _slept <- result.try(workflow.sleep(duration.minutes(5)) |> result_map_engine_error)
  use decision <-
    result.try(workflow.receive(example_decision_signal()) |> result_map_receive_error)
  use handle <-
    result.try(
      workflow.spawn(
        "checkout-child",
        checkout_workflow,
        ChargeRequest(order_id: input.id, cents: input.cents),
        charge_request_codec(),
        charge_receipt_codec(),
        workflow_error_codec(),
      )
      |> result_map_engine_error,
    )
  use child_receipt <-
    result.try(child.await(handle) |> result_map_child_error)
  use fanout_receipts <-
    result.try(
      workflow.all([
        example_charge_activity("example-fanout", input.id <> "-a", input.cents),
        example_charge_activity("example-fanout", input.id <> "-b", input.cents),
      ])
      |> result_map_activity_error,
    )

  Ok(ExampleSummary(
    order_id: input.id,
    run_receipt_id: run_receipt.id,
    child_receipt_id: child_receipt.id,
    fanout_receipt_ids: receipt_ids(fanout_receipts),
    approved: decision.approved,
    observed_at_milliseconds: workflow.timestamp_to_milliseconds(observed_at),
  ))
}

fn example_decision_signal() -> signal.SignalRef(ExampleDecision) {
  signal.new("example-approval", example_decision_codec())
}

fn receipt_ids(receipts: List(ChargeReceipt)) -> List(String) {
  case receipts {
    [] -> []
    [first, ..rest] -> [first.id, ..receipt_ids(rest)]
  }
}

fn result_map_engine_error(
  value: Result(inner, error.EngineError),
) -> Result(inner, String) {
  case value {
    Ok(inner) -> Ok(inner)
    Error(_) -> Error("engine failure")
  }
}

fn result_map_activity_error(
  value: Result(inner, error.ActivityError),
) -> Result(inner, String) {
  case value {
    Ok(inner) -> Ok(inner)
    Error(_) -> Error("activity failure")
  }
}

fn result_map_receive_error(
  value: Result(inner, error.ReceiveError),
) -> Result(inner, String) {
  case value {
    Ok(inner) -> Ok(inner)
    Error(_) -> Error("receive failure")
  }
}

fn result_map_child_error(
  value: Result(inner, error.ChildError(String)),
) -> Result(inner, String) {
  case value {
    Ok(inner) -> Ok(inner)
    Error(_) -> Error("child failure")
  }
}

fn example_charge_activity(
  name: String,
  order_id: String,
  cents: Int,
) -> activity.Activity(ChargeRequest, ChargeReceipt) {
  activity.new(
    name,
    ChargeRequest(order_id: order_id, cents: cents),
    charge_request_codec(),
    charge_receipt_codec(),
    charge_payment,
  )
}

fn charge_activity(
  name: String,
  order_id: String,
) -> activity.Activity(ChargeRequest, ChargeReceipt) {
  activity.new(
    name,
    ChargeRequest(order_id: order_id, cents: 700),
    charge_request_codec(),
    charge_receipt_codec(),
    charge_payment,
  )
}

fn checkout_workflow(request: ChargeRequest) -> Result(ChargeReceipt, String) {
  Ok(ChargeReceipt(id: "receipt-" <> request.order_id, approved: True))
}

fn charge_request_codec() -> codec.Codec(ChargeRequest) {
  codec.json_codec(charge_request_to_json, charge_request_decoder())
}

fn example_order_codec() -> codec.Codec(ExampleOrder) {
  codec.json_codec(example_order_to_json, example_order_decoder())
}

fn example_order_to_json(order: ExampleOrder) -> json.Json {
  json.object([
    #("id", json.string(order.id)),
    #("cents", json.int(order.cents)),
  ])
}

fn example_order_decoder() -> decode.Decoder(ExampleOrder) {
  use id <- decode.field("id", decode.string)
  use cents <- decode.field("cents", decode.int)
  decode.success(ExampleOrder(id: id, cents: cents))
}

fn example_decision_codec() -> codec.Codec(ExampleDecision) {
  codec.json_codec(example_decision_to_json, example_decision_decoder())
}

fn example_decision_to_json(decision: ExampleDecision) -> json.Json {
  json.object([#("approved", json.bool(decision.approved))])
}

fn example_decision_decoder() -> decode.Decoder(ExampleDecision) {
  use approved <- decode.field("approved", decode.bool)
  decode.success(ExampleDecision(approved: approved))
}

fn example_summary_codec() -> codec.Codec(ExampleSummary) {
  codec.json_codec(example_summary_to_json, example_summary_decoder())
}

fn example_summary_to_json(summary: ExampleSummary) -> json.Json {
  json.object([
    #("order_id", json.string(summary.order_id)),
    #("run_receipt_id", json.string(summary.run_receipt_id)),
    #("child_receipt_id", json.string(summary.child_receipt_id)),
    #("fanout_receipt_ids", json.array(summary.fanout_receipt_ids, json.string)),
    #("approved", json.bool(summary.approved)),
    #("observed_at_milliseconds", json.int(summary.observed_at_milliseconds)),
  ])
}

fn example_summary_decoder() -> decode.Decoder(ExampleSummary) {
  use order_id <- decode.field("order_id", decode.string)
  use run_receipt_id <- decode.field("run_receipt_id", decode.string)
  use child_receipt_id <- decode.field("child_receipt_id", decode.string)
  use fanout_receipt_ids <-
    decode.field("fanout_receipt_ids", decode.list(decode.string))
  use approved <- decode.field("approved", decode.bool)
  use observed_at_milliseconds <-
    decode.field("observed_at_milliseconds", decode.int)
  decode.success(ExampleSummary(
    order_id: order_id,
    run_receipt_id: run_receipt_id,
    child_receipt_id: child_receipt_id,
    fanout_receipt_ids: fanout_receipt_ids,
    approved: approved,
    observed_at_milliseconds: observed_at_milliseconds,
  ))
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

fn workflow_error_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
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

fn order_decoder() -> decode.Decoder(Order) {
  use id <- decode.field("id", decode.string)
  use items <- decode.field("items", decode.list(line_item_decoder()))
  decode.success(Order(id: id, items: items))
}

fn line_item_decoder() -> decode.Decoder(LineItem) {
  use sku <- decode.field("sku", decode.string)
  use quantity <- decode.field("quantity", decode.int)
  decode.success(LineItem(sku: sku, quantity: quantity))
}
