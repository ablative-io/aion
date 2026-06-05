//// Focused aion_client SDK tests and seven-operation example surface checks.

import aion_client
import aion_client/error
import aion_client/handle as workflow_handle
import aion_client/payload
import aion_client/stream
import gleam/dynamic/decode
import gleam/option.{Some}
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

pub fn seven_operations_example_flow_test() {
  let config =
    aion_client.Config(
      endpoint: "http://127.0.0.1:8080",
      bearer_token: Some("dev-token"),
      namespace: "conformance",
      tls: False,
    )
  let assert Ok(client) = aion_client.with_transport(config, example_transport())

  let assert Ok(handle) =
    aion_client.start(
      client,
      aion_client.StartOptions(
        workflow_id: "echo-example",
        workflow_type: "conformance.echo",
        task_queue: "conformance",
        idempotency_key: Some("gleam-seven-operations-example"),
      ),
      #("hello", 1),
      fn(input) {
        let #(message, counter) = input
        json.object([
          #("message", json.string(message)),
          #("counter", json.int(counter)),
        ])
      },
    )

  let assert Ok(Nil) =
    workflow_handle.signal(handle, "record", "signal-observed", fn(value) {
      json.object([#("value", json.string(value))])
    })

  let assert Ok("signal-observed") =
    workflow_handle.query(handle, "state", Nil, fn(_) { json.null() }, decode.string)

  let assert Ok(summaries) =
    aion_client.list(client, aion_client.ListOptions(namespace: Some("conformance")))
  summaries |> aion_client.workflow_ids |> should.equal(["echo-example"])

  let assert Ok(description) = workflow_handle.describe(handle)
  description
  |> should.equal(aion_client.WorkflowDescription(
    workflow_id: "echo-example",
    run_id: "run-example",
    workflow_type: "conformance.echo",
    status: "running",
  ))

  let assert Ok(Nil) = workflow_handle.cancel(handle, "seven-operations example requested cancellation")

  workflow_handle.subscribe(handle, decode.string)
  |> stream.collect
  |> should.equal([stream.StreamError(error.Unavailable)])

  let subscribe_fixture =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        0 -> [
          stream.Frame(sequence: 1, payload: payload.encode("started", json.string)),
          stream.TransientDisconnect,
        ]
        2 -> [
          stream.Frame(sequence: 2, payload: payload.encode("cancelled", json.string)),
          stream.EndOfStream,
        ]
        _ -> [stream.EndOfStream]
      }
    })

  subscribe_fixture
  |> stream.subscribe_with_stub(decode.string)
  |> stream.collect
  |> should.equal([
    stream.EventItem(stream.Event(sequence: 1, payload: "started")),
    stream.EventItem(stream.Event(sequence: 2, payload: "cancelled")),
    stream.StreamEnd,
  ])

  let conflict =
    aion_client.start_raw(
      client,
      aion_client.StartOptions(
        workflow_id: "echo-example",
        workflow_type: "conformance.other",
        task_queue: "conformance",
        idempotency_key: Some("gleam-seven-operations-example"),
      ),
      payload.Payload(content_type: payload.json_content_type, bytes: "{}"),
    )
  case conflict {
    Error(actual) -> should.equal(actual, error.AlreadyExists)
    Ok(_) -> should.equal(False, True)
  }
}

fn example_transport() -> aion_client.Transport {
  aion_client.Transport(
    start: example_start,
    signal: example_signal,
    query: example_query,
    cancel: example_cancel,
    list: example_list,
    describe: example_describe,
  )
}

fn example_start(
  _config: aion_client.Config,
  request: aion_client.StartRequest,
) -> Result(aion_client.StartResponse, error.Error) {
  let aion_client.StartRequest(options: options, ..) = request
  let aion_client.StartOptions(workflow_type: workflow_type, idempotency_key: key, ..) = options
  case key == Some("gleam-seven-operations-example") && workflow_type != "conformance.echo" {
    True -> Error(error.AlreadyExists)
    False -> Ok(aion_client.StartResponse(workflow_id: "echo-example", run_id: "run-example"))
  }
}

fn example_signal(
  _config: aion_client.Config,
  _request: aion_client.SignalRequest,
) -> Result(Nil, error.Error) {
  Ok(Nil)
}

fn example_query(
  _config: aion_client.Config,
  _request: aion_client.QueryRequest,
) -> Result(payload.Payload, error.Error) {
  Ok(payload.encode("signal-observed", json.string))
}

fn example_cancel(
  _config: aion_client.Config,
  _request: aion_client.CancelRequest,
) -> Result(Nil, error.Error) {
  Ok(Nil)
}

fn example_list(
  _config: aion_client.Config,
  _request: aion_client.ListRequest,
) -> Result(List(aion_client.WorkflowSummary), error.Error) {
  Ok([
    aion_client.WorkflowSummary(
      workflow_id: "echo-example",
      run_id: "run-example",
      workflow_type: "conformance.echo",
      status: "running",
    ),
  ])
}

fn example_describe(
  _config: aion_client.Config,
  _request: aion_client.DescribeRequest,
) -> Result(aion_client.WorkflowDescription, error.Error) {
  Ok(aion_client.WorkflowDescription(
    workflow_id: "echo-example",
    run_id: "run-example",
    workflow_type: "conformance.echo",
    status: "running",
  ))
}
