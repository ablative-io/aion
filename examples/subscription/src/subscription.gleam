//// Long-lived monthly subscription workflow example.
////
//// The workflow waits for a durable billing timer, accepts typed plan-change
//// signals while it waits, bills the subscriber for the active plan, and rotates
//// history with continue-as-new after a configured number of cycles per run.

import aion/activity
import aion/codec
import aion/duration
import aion/error
import aion/signal
import aion/workflow
import aion/workflow/timer
import gleam/dynamic/decode
import gleam/int
import gleam/json

pub type SubscriptionInput {
  SubscriptionInput(
    subscriber_id: String,
    subscriber_email: String,
    plan: String,
    current_cycle: Int,
    billing_period_seconds: Int,
    max_cycles: Int,
    cycles_in_run: Int,
  )
}

pub type SubscriptionSummary {
  SubscriptionSummary(
    subscriber_id: String,
    plan: String,
    next_cycle: Int,
    cycles_in_run: Int,
    status: String,
  )
}

pub type BillSubscriberInput {
  BillSubscriberInput(subscriber_id: String, subscriber_email: String, plan: String, cycle: Int)
}

pub type BillResult {
  BillResult(subscriber_id: String, plan: String, cycle: Int, invoice_id: String, status: String)
}

pub type PlanChange {
  Upgrade(plan: String)
  Downgrade(plan: String)
}

pub type WorkflowError {
  ActivityFailed(message: String)
  TimerFailed(message: String)
  SignalFailed(message: String)
  ContinueAsNewFailed(message: String)
  InvalidConfiguration(message: String)
}

pub fn definition() ->
  workflow.WorkflowDefinition(SubscriptionInput, SubscriptionSummary, WorkflowError) {
  workflow.define(
    "subscription",
    subscription_input_codec(),
    subscription_summary_codec(),
    workflow_error_codec(),
    run,
  )
}

pub fn plan_change_signal() -> signal.SignalRef(PlanChange) {
  signal.new("plan_change", plan_change_codec())
}

pub fn run(input: SubscriptionInput) -> Result(SubscriptionSummary, WorkflowError) {
  case validate_input(input) {
    Ok(valid_input) -> billing_loop(valid_input)
    Error(configuration_error) -> Error(configuration_error)
  }
}

fn validate_input(input: SubscriptionInput) -> Result(SubscriptionInput, WorkflowError) {
  case input.billing_period_seconds > 0, input.max_cycles > 0, input.cycles_in_run >= 0 {
    True, True, True -> Ok(input)
    False, _, _ -> Error(InvalidConfiguration("billing_period_seconds must be greater than zero"))
    _, False, _ -> Error(InvalidConfiguration("max_cycles must be greater than zero"))
    _, _, False -> Error(InvalidConfiguration("cycles_in_run must not be negative"))
  }
}

fn billing_loop(input: SubscriptionInput) -> Result(SubscriptionSummary, WorkflowError) {
  case wait_for_billing_period(input, input.billing_period_seconds * 1000) {
    Ok(updated_input) -> bill_cycle(updated_input)
    Error(wait_error) -> Error(wait_error)
  }
}

fn wait_for_billing_period(
  input: SubscriptionInput,
  remaining_milliseconds: Int,
) -> Result(SubscriptionInput, WorkflowError) {
  case workflow.now() {
    Ok(started_at) -> wait_for_billing_period_from(input, remaining_milliseconds, started_at)
    Error(engine_error) -> Error(TimerFailed(engine_error_message(engine_error)))
  }
}

fn wait_for_billing_period_from(
  input: SubscriptionInput,
  remaining_milliseconds: Int,
  started_at: workflow.Timestamp,
) -> Result(SubscriptionInput, WorkflowError) {
  case timer.with_timeout(
    fn() { signal.receive(plan_change_signal()) },
    duration.milliseconds(remaining_milliseconds),
  ) {
    Ok(change) -> {
      let updated_input = apply_plan_change(input, change)
      case remaining_after_signal(started_at, remaining_milliseconds) {
        Ok(next_remaining) -> wait_for_billing_period(updated_input, next_remaining)
        Error(timer_error) -> Error(timer_error)
      }
    }
    Error(error.TimedOutError(error.TimedOut(message: _))) -> Ok(input)
    Error(error.InnerError(receive_error)) ->
      Error(SignalFailed(receive_error_message(receive_error)))
  }
}

fn remaining_after_signal(
  started_at: workflow.Timestamp,
  remaining_milliseconds: Int,
) -> Result(Int, WorkflowError) {
  case workflow.now() {
    Ok(received_at) -> {
      let elapsed_milliseconds =
        workflow.timestamp_to_milliseconds(received_at)
        - workflow.timestamp_to_milliseconds(started_at)
      let next_remaining = remaining_milliseconds - elapsed_milliseconds
      case next_remaining > 0 {
        True -> Ok(next_remaining)
        False -> Ok(0)
      }
    }
    Error(engine_error) -> Error(TimerFailed(engine_error_message(engine_error)))
  }
}

fn apply_plan_change(input: SubscriptionInput, change: PlanChange) -> SubscriptionInput {
  let next_plan = case change {
    Upgrade(plan) -> plan
    Downgrade(plan) -> plan
  }

  SubscriptionInput(
    subscriber_id: input.subscriber_id,
    subscriber_email: input.subscriber_email,
    plan: next_plan,
    current_cycle: input.current_cycle,
    billing_period_seconds: input.billing_period_seconds,
    max_cycles: input.max_cycles,
    cycles_in_run: input.cycles_in_run,
  )
}

fn bill_cycle(input: SubscriptionInput) -> Result(SubscriptionSummary, WorkflowError) {
  case workflow.run(bill_subscriber_activity(BillSubscriberInput(
    subscriber_id: input.subscriber_id,
    subscriber_email: input.subscriber_email,
    plan: input.plan,
    cycle: input.current_cycle,
  ))) {
    Ok(_result) -> rotate_or_continue(input)
    Error(activity_error) -> Error(ActivityFailed(activity_error_message(activity_error)))
  }
}

fn rotate_or_continue(input: SubscriptionInput) -> Result(SubscriptionSummary, WorkflowError) {
  let next_input = SubscriptionInput(
    subscriber_id: input.subscriber_id,
    subscriber_email: input.subscriber_email,
    plan: input.plan,
    current_cycle: input.current_cycle + 1,
    billing_period_seconds: input.billing_period_seconds,
    max_cycles: input.max_cycles,
    cycles_in_run: input.cycles_in_run + 1,
  )

  case next_input.cycles_in_run >= next_input.max_cycles {
    True -> continue_with_fresh_history(reset_cycles_in_run(next_input))
    False -> billing_loop(next_input)
  }
}

fn continue_with_fresh_history(
  next_input: SubscriptionInput,
) -> Result(SubscriptionSummary, WorkflowError) {
  case workflow.continue_as_new(next_input) {
    Ok(_) ->
      Ok(SubscriptionSummary(
        subscriber_id: next_input.subscriber_id,
        plan: next_input.plan,
        next_cycle: next_input.current_cycle,
        cycles_in_run: next_input.cycles_in_run,
        status: "continued_as_new",
      ))
    Error(engine_error) -> Error(ContinueAsNewFailed(engine_error_message(engine_error)))
  }
}

fn reset_cycles_in_run(input: SubscriptionInput) -> SubscriptionInput {
  SubscriptionInput(
    subscriber_id: input.subscriber_id,
    subscriber_email: input.subscriber_email,
    plan: input.plan,
    current_cycle: input.current_cycle,
    billing_period_seconds: input.billing_period_seconds,
    max_cycles: input.max_cycles,
    cycles_in_run: 0,
  )
}

fn bill_subscriber_activity(
  input: BillSubscriberInput,
) -> activity.Activity(BillSubscriberInput, BillResult) {
  activity.new(
    "bill_subscriber",
    input,
    bill_subscriber_input_codec(),
    bill_result_codec(),
    local_bill_subscriber,
  )
}

fn local_bill_subscriber(input: BillSubscriberInput) -> Result(BillResult, error.ActivityError) {
  Ok(BillResult(
    subscriber_id: input.subscriber_id,
    plan: input.plan,
    cycle: input.cycle,
    invoice_id: "inv-" <> input.subscriber_id <> "-" <> int.to_string(input.cycle),
    status: "billed",
  ))
}

fn subscription_input_codec() -> codec.Codec(SubscriptionInput) {
  codec.json_codec(subscription_input_to_json, subscription_input_decoder())
}

fn subscription_input_to_json(input: SubscriptionInput) -> json.Json {
  json.object([
    #("subscriber_id", json.string(input.subscriber_id)),
    #("subscriber_email", json.string(input.subscriber_email)),
    #("plan", json.string(input.plan)),
    #("current_cycle", json.int(input.current_cycle)),
    #("billing_period_seconds", json.int(input.billing_period_seconds)),
    #("max_cycles", json.int(input.max_cycles)),
    #("cycles_in_run", json.int(input.cycles_in_run)),
  ])
}

fn subscription_input_decoder() -> decode.Decoder(SubscriptionInput) {
  use subscriber_id <- decode.field("subscriber_id", decode.string)
  use subscriber_email <- decode.field("subscriber_email", decode.string)
  use plan <- decode.field("plan", decode.string)
  use current_cycle <- decode.field("current_cycle", decode.int)
  use billing_period_seconds <- decode.field("billing_period_seconds", decode.int)
  use max_cycles <- decode.field("max_cycles", decode.int)
  use cycles_in_run <- decode.field("cycles_in_run", decode.int)
  decode.success(SubscriptionInput(
    subscriber_id: subscriber_id,
    subscriber_email: subscriber_email,
    plan: plan,
    current_cycle: current_cycle,
    billing_period_seconds: billing_period_seconds,
    max_cycles: max_cycles,
    cycles_in_run: cycles_in_run,
  ))
}

fn subscription_summary_codec() -> codec.Codec(SubscriptionSummary) {
  codec.json_codec(subscription_summary_to_json, subscription_summary_decoder())
}

fn subscription_summary_to_json(summary: SubscriptionSummary) -> json.Json {
  json.object([
    #("subscriber_id", json.string(summary.subscriber_id)),
    #("plan", json.string(summary.plan)),
    #("next_cycle", json.int(summary.next_cycle)),
    #("cycles_in_run", json.int(summary.cycles_in_run)),
    #("status", json.string(summary.status)),
  ])
}

fn subscription_summary_decoder() -> decode.Decoder(SubscriptionSummary) {
  use subscriber_id <- decode.field("subscriber_id", decode.string)
  use plan <- decode.field("plan", decode.string)
  use next_cycle <- decode.field("next_cycle", decode.int)
  use cycles_in_run <- decode.field("cycles_in_run", decode.int)
  use status <- decode.field("status", decode.string)
  decode.success(SubscriptionSummary(
    subscriber_id: subscriber_id,
    plan: plan,
    next_cycle: next_cycle,
    cycles_in_run: cycles_in_run,
    status: status,
  ))
}

fn bill_subscriber_input_codec() -> codec.Codec(BillSubscriberInput) {
  codec.json_codec(bill_subscriber_input_to_json, bill_subscriber_input_decoder())
}

fn bill_subscriber_input_to_json(input: BillSubscriberInput) -> json.Json {
  json.object([
    #("subscriber_id", json.string(input.subscriber_id)),
    #("subscriber_email", json.string(input.subscriber_email)),
    #("plan", json.string(input.plan)),
    #("cycle", json.int(input.cycle)),
  ])
}

fn bill_subscriber_input_decoder() -> decode.Decoder(BillSubscriberInput) {
  use subscriber_id <- decode.field("subscriber_id", decode.string)
  use subscriber_email <- decode.field("subscriber_email", decode.string)
  use plan <- decode.field("plan", decode.string)
  use cycle <- decode.field("cycle", decode.int)
  decode.success(BillSubscriberInput(
    subscriber_id: subscriber_id,
    subscriber_email: subscriber_email,
    plan: plan,
    cycle: cycle,
  ))
}

fn bill_result_codec() -> codec.Codec(BillResult) {
  codec.json_codec(bill_result_to_json, bill_result_decoder())
}

fn bill_result_to_json(result: BillResult) -> json.Json {
  json.object([
    #("subscriber_id", json.string(result.subscriber_id)),
    #("plan", json.string(result.plan)),
    #("cycle", json.int(result.cycle)),
    #("invoice_id", json.string(result.invoice_id)),
    #("status", json.string(result.status)),
  ])
}

fn bill_result_decoder() -> decode.Decoder(BillResult) {
  use subscriber_id <- decode.field("subscriber_id", decode.string)
  use plan <- decode.field("plan", decode.string)
  use cycle <- decode.field("cycle", decode.int)
  use invoice_id <- decode.field("invoice_id", decode.string)
  use status <- decode.field("status", decode.string)
  decode.success(BillResult(
    subscriber_id: subscriber_id,
    plan: plan,
    cycle: cycle,
    invoice_id: invoice_id,
    status: status,
  ))
}

fn plan_change_codec() -> codec.Codec(PlanChange) {
  codec.json_codec(plan_change_to_json, plan_change_decoder())
}

fn plan_change_to_json(change: PlanChange) -> json.Json {
  case change {
    Upgrade(plan) ->
      json.object([
        #("direction", json.string("upgrade")),
        #("plan", json.string(plan)),
      ])
    Downgrade(plan) ->
      json.object([
        #("direction", json.string("downgrade")),
        #("plan", json.string(plan)),
      ])
  }
}

fn plan_change_decoder() -> decode.Decoder(PlanChange) {
  use direction <- decode.field("direction", decode.string)
  use plan <- decode.field("plan", decode.string)
  case direction {
    "upgrade" -> decode.success(Upgrade(plan: plan))
    "downgrade" -> decode.success(Downgrade(plan: plan))
    _ -> decode.failure(Upgrade(plan: ""), expected: "plan_change direction upgrade or downgrade")
  }
}

fn workflow_error_codec() -> codec.Codec(WorkflowError) {
  codec.json_codec(workflow_error_to_json, workflow_error_decoder())
}

fn workflow_error_to_json(workflow_error: WorkflowError) -> json.Json {
  case workflow_error {
    ActivityFailed(message) -> error_json("activity_failed", message)
    TimerFailed(message) -> error_json("timer_failed", message)
    SignalFailed(message) -> error_json("signal_failed", message)
    ContinueAsNewFailed(message) -> error_json("continue_as_new_failed", message)
    InvalidConfiguration(message) -> error_json("invalid_configuration", message)
  }
}

fn error_json(error_type: String, message: String) -> json.Json {
  json.object([
    #("type", json.string(error_type)),
    #("message", json.string(message)),
  ])
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use error_type <- decode.field("type", decode.string)
  use message <- decode.field("message", decode.string)
  case error_type {
    "timer_failed" -> decode.success(TimerFailed(message: message))
    "signal_failed" -> decode.success(SignalFailed(message: message))
    "continue_as_new_failed" -> decode.success(ContinueAsNewFailed(message: message))
    "invalid_configuration" -> decode.success(InvalidConfiguration(message: message))
    _ -> decode.success(ActivityFailed(message: message))
  }
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(message: message)) ->
      message
    error.ActivityEngineFailure(message: message) -> message
  }
}

fn receive_error_message(receive_error: error.ReceiveError) -> String {
  case receive_error {
    error.ReceiveDecodeFailed(_) -> "signal payload could not be decoded"
    error.UnknownSignal(name: name) -> "unknown signal: " <> name
    error.ReceiveCancelled(error.Cancelled(reason: reason)) -> reason
    error.ReceiveNonDeterministic(error.NonDeterminismViolation(message: message)) ->
      message
    error.ReceiveEngineFailure(message: message) -> message
  }
}

fn engine_error_message(engine_error: error.EngineError) -> String {
  case engine_error {
    error.EngineFailure(message: message) -> message
  }
}
