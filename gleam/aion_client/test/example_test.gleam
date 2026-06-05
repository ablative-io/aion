//// Focused aion_client SDK tests and seven-operation example surface checks.

import aion_client/error
import aion_client/payload
import aion_client/stream
import gleam/dynamic/decode
import gleam/json
import gleeunit
import gleeunit/should

pub fn main() {
  gleeunit.main()
}

pub fn error_mapping_covers_contract_cases_test() {
  error.from_wire(error.WireNotFound, "missing")
  |> should.equal(error.NotFound)

  error.from_http_status(409, "conflict")
  |> should.equal(error.AlreadyExists)

  error.from_wire(error.WireQueryTimeout, "timed out")
  |> should.equal(error.QueryTimeout)

  error.transport_failure()
  |> should.equal(error.Unavailable)
}

pub fn payload_round_trips_json_value_test() {
  let encoded = payload.encode(42, json.int)

  encoded
  |> payload.decode(decode.int)
  |> should.equal(Ok(42))
}

pub fn payload_decode_failure_is_invalid_argument_test() {
  payload.Payload(content_type: payload.json_content_type, bytes: "{invalid")
  |> payload.decode(decode.int)
  |> should.equal(Error(error.InvalidArgument))
}

pub fn stream_resumes_after_drop_without_duplicates_test() {
  let transport =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        0 -> [
          stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
          stream.Frame(sequence: 2, payload: payload.encode(2, json.int)),
          stream.TransientDisconnect,
        ]
        3 -> [
          stream.Frame(sequence: 2, payload: payload.encode(2, json.int)),
          stream.Frame(sequence: 3, payload: payload.encode(3, json.int)),
          stream.EndOfStream,
        ]
        _ -> [stream.EndOfStream]
      }
    })

  transport
  |> stream.subscribe_with_stub(decode.int)
  |> stream.collect
  |> should.equal([
    stream.EventItem(stream.Event(sequence: 1, payload: 1)),
    stream.EventItem(stream.Event(sequence: 2, payload: 2)),
    stream.EventItem(stream.Event(sequence: 3, payload: 3)),
    stream.StreamEnd,
  ])
}

pub fn stream_terminal_failure_is_item_test() {
  let transport =
    stream.StubTransport(open: fn(_) {
      [
        stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
        stream.TerminalFailure(error.QueryFailed),
      ]
    })

  transport
  |> stream.subscribe_with_stub(decode.int)
  |> stream.collect
  |> should.equal([
    stream.EventItem(stream.Event(sequence: 1, payload: 1)),
    stream.StreamError(error.QueryFailed),
  ])
}
