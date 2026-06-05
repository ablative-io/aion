//// aion_flow foundational primitive tests.

import aion/codec
import aion/duration
import aion/error
import gleam/dynamic/decode
import gleam/json
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
  let retryable = error.retryable_with_details("network failed", "{\"attempt\":1}")
  let terminal = error.terminal_with_details("invalid order", "{\"field\":\"id\"}")

  case retryable {
    error.Retryable(message: message, details: details) -> {
      message |> should.equal("network failed")
      details |> should.equal("{\"attempt\":1}")
    }
    error.Terminal(_, _) -> should.fail()
  }

  case terminal {
    error.Terminal(message: message, details: details) -> {
      message |> should.equal("invalid order")
      details |> should.equal("{\"field\":\"id\"}")
    }
    error.Retryable(_, _) -> should.fail()
  }
}

pub fn activity_error_helpers_use_empty_details_test() {
  case error.retryable("temporary outage") {
    error.Retryable(message: message, details: details) -> {
      message |> should.equal("temporary outage")
      details |> should.equal("")
    }
    error.Terminal(_, _) -> should.fail()
  }

  case error.terminal("bad request") {
    error.Terminal(message: message, details: details) -> {
      message |> should.equal("bad request")
      details |> should.equal("")
    }
    error.Retryable(_, _) -> should.fail()
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
