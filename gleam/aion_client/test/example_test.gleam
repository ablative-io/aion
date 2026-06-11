//// Focused aion_client SDK tests and seven-operation example surface checks.

import aion_client
import aion_client/error
import aion_client/handle as workflow_handle
import aion_client/payload
import aion_client/stream
import gleam/dynamic/decode
import gleam/json
import gleam/option.{None, Some}
import gleeunit/should

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

pub fn namespace_denied_is_distinct_from_unauthenticated_test() {
  error.from_wire(error.WireNamespaceDenied, "namespace tenants/acme denied")
  |> should.equal(error.NamespaceDenied("namespace tenants/acme denied"))

  error.from_http_status(403, "namespace tenants/acme denied")
  |> should.equal(error.NamespaceDenied("namespace tenants/acme denied"))

  error.from_http_status(401, "bearer token rejected")
  |> should.equal(error.Unauthenticated)
}

pub fn wire_codes_map_exactly_onto_taxonomy_test() {
  error.from_wire(error.WireNotFound, "d")
  |> should.equal(error.NotFound)
  error.from_wire(error.WireNamespaceDenied, "d")
  |> should.equal(error.NamespaceDenied("d"))
  error.from_wire(error.WireSequenceConflict, "d")
  |> should.equal(error.Server("d"))
  error.from_wire(error.WireUnknownQuery, "d")
  |> should.equal(error.InvalidArgument)
  error.from_wire(error.WireQueryTimeout, "d")
  |> should.equal(error.QueryTimeout)
  error.from_wire(error.WireNotRunning, "d")
  |> should.equal(error.InvalidArgument)
  error.from_wire(error.WireLagged, "d")
  |> should.equal(error.Unavailable)
  error.from_wire(error.WireInvalidInput, "d")
  |> should.equal(error.InvalidArgument)
  error.from_wire(error.WireBackend, "d")
  |> should.equal(error.Server("d"))
  error.from_wire(error.WireQueryFailed, "d")
  |> should.equal(error.QueryFailed)
  error.from_wire(error.WireUnknown("mystery"), "d")
  |> should.equal(error.Server("d"))
}

pub fn sequence_conflict_is_server_fault_not_already_exists_test() {
  // sequence_conflict on the wire is the server's internal double-writer-bug
  // signal, never an idempotency outcome.
  error.from_wire(error.WireSequenceConflict, "double-writer detected")
  |> should.equal(error.Server("double-writer detected"))

  // HTTP 409 (idempotent start conflict) still maps to AlreadyExists.
  error.from_http_status(409, "workflow already started")
  |> should.equal(error.AlreadyExists)
}

pub fn gateway_statuses_are_retryable_unavailable_test() {
  error.from_http_status(502, "bad gateway")
  |> should.equal(error.Unavailable)
  error.from_http_status(503, "service unavailable")
  |> should.equal(error.Unavailable)
  error.from_http_status(504, "gateway timeout")
  |> should.equal(error.Unavailable)

  // 500 and unrecognised statuses remain server faults.
  error.from_http_status(500, "internal")
  |> should.equal(error.Server("internal"))
  error.from_http_status(599, "unrecognised")
  |> should.equal(error.Server("unrecognised"))
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
  // Fresh subscribe carries no resume field (None); the reconnect after the
  // transient disconnect must ask for exactly Some(3) = last delivered + 1.
  // Any other cursor hits the strict fallback and fails the assertion below.
  let transport =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        None -> [
          stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
          stream.Frame(sequence: 2, payload: payload.encode(2, json.int)),
          stream.TransientDisconnect,
        ]
        Some(3) -> [
          stream.Frame(sequence: 2, payload: payload.encode(2, json.int)),
          stream.Frame(sequence: 3, payload: payload.encode(3, json.int)),
          stream.EndOfStream,
        ]
        Some(_) -> [stream.TerminalFailure(error.InvalidArgument)]
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

pub fn fresh_subscribe_sends_no_resume_cursor_test() {
  // A first subscription has delivered nothing, so the request must carry no
  // resume field at all (None) — never Some(0), never Some(1). A request
  // with any cursor present surfaces as the InvalidArgument item below and
  // fails the exact-list assertion.
  let transport =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        None -> [
          stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
          stream.EndOfStream,
        ]
        Some(_) -> [stream.TerminalFailure(error.InvalidArgument)]
      }
    })

  transport
  |> stream.subscribe_with_stub(decode.int)
  |> stream.collect
  |> should.equal([
    stream.EventItem(stream.Event(sequence: 1, payload: 1)),
    stream.StreamEnd,
  ])
}

pub fn resume_after_disconnect_requests_last_delivered_plus_one_test() {
  // resume_from_seq is the FIRST sequence wanted: after delivering through
  // seq 2 the reconnect must request exactly Some(3). The stub replays
  // nothing for any other cursor, so an off-by-one (Some(2) re-requesting
  // the delivered event, or Some(4) skipping one) fails loudly.
  let transport =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        None -> [
          stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
          stream.Frame(sequence: 2, payload: payload.encode(2, json.int)),
          stream.TransientDisconnect,
        ]
        Some(3) -> [
          stream.Frame(sequence: 3, payload: payload.encode(3, json.int)),
          stream.EndOfStream,
        ]
        Some(_) -> [stream.TerminalFailure(error.InvalidArgument)]
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

pub fn gap_in_replayed_stream_surfaces_unavailable_test() {
  // The resume replay asked for first-wanted seq 2 but the stream jumps to
  // seq 3: an event was lost. That must surface as Unavailable, never a
  // silent skip-ahead.
  let transport =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        None -> [
          stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
          stream.TransientDisconnect,
        ]
        Some(2) -> [
          stream.Frame(sequence: 3, payload: payload.encode(3, json.int)),
          stream.EndOfStream,
        ]
        Some(_) -> [stream.TerminalFailure(error.InvalidArgument)]
      }
    })

  transport
  |> stream.subscribe_with_stub(decode.int)
  |> stream.collect
  |> should.equal([
    stream.EventItem(stream.Event(sequence: 1, payload: 1)),
    stream.StreamError(error.Unavailable),
  ])
}

pub fn duplicate_replay_is_deduped_exactly_once_test() {
  // The server may re-send already-delivered frames on reconnect (here the
  // whole prefix 1..3 ahead of the new seq 4). Each sequence appears in the
  // delivered list exactly once and in order — the exact-list assertion
  // rejects both re-delivery and dropped events.
  let transport =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        None -> [
          stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
          stream.Frame(sequence: 2, payload: payload.encode(2, json.int)),
          stream.Frame(sequence: 3, payload: payload.encode(3, json.int)),
          stream.TransientDisconnect,
        ]
        Some(4) -> [
          stream.Frame(sequence: 1, payload: payload.encode(1, json.int)),
          stream.Frame(sequence: 2, payload: payload.encode(2, json.int)),
          stream.Frame(sequence: 3, payload: payload.encode(3, json.int)),
          stream.Frame(sequence: 4, payload: payload.encode(4, json.int)),
          stream.EndOfStream,
        ]
        Some(_) -> [stream.TerminalFailure(error.InvalidArgument)]
      }
    })

  transport
  |> stream.subscribe_with_stub(decode.int)
  |> stream.collect
  |> should.equal([
    stream.EventItem(stream.Event(sequence: 1, payload: 1)),
    stream.EventItem(stream.Event(sequence: 2, payload: 2)),
    stream.EventItem(stream.Event(sequence: 3, payload: 3)),
    stream.EventItem(stream.Event(sequence: 4, payload: 4)),
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
  let assert Ok(client) =
    aion_client.with_transport(config, example_transport())

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
    workflow_handle.query(
      handle,
      "state",
      Nil,
      fn(_) { json.null() },
      decode.string,
    )

  let assert Ok(summaries) =
    aion_client.list(
      client,
      aion_client.ListOptions(namespace: Some("conformance")),
    )
  summaries |> aion_client.workflow_ids |> should.equal(["echo-example"])

  let assert Ok(description) = workflow_handle.describe(handle)
  description
  |> should.equal(aion_client.WorkflowDescription(
    workflow_id: "echo-example",
    run_id: "run-example",
    workflow_type: "conformance.echo",
    status: "running",
  ))

  let assert Ok(Nil) =
    workflow_handle.cancel(
      handle,
      "seven-operations example requested cancellation",
    )

  workflow_handle.subscribe(handle, decode.string)
  |> stream.collect
  |> should.equal([stream.StreamError(error.Unavailable)])

  let subscribe_fixture =
    stream.StubTransport(open: fn(cursor) {
      case cursor {
        None -> [
          stream.Frame(
            sequence: 1,
            payload: payload.encode("started", json.string),
          ),
          stream.TransientDisconnect,
        ]
        Some(2) -> [
          stream.Frame(
            sequence: 2,
            payload: payload.encode("cancelled", json.string),
          ),
          stream.EndOfStream,
        ]
        Some(_) -> [stream.TerminalFailure(error.InvalidArgument)]
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
  let aion_client.StartOptions(
    workflow_type: workflow_type,
    idempotency_key: key,
    ..,
  ) = options
  case
    key == Some("gleam-seven-operations-example")
    && workflow_type != "conformance.echo"
  {
    True -> Error(error.AlreadyExists)
    False ->
      Ok(aion_client.StartResponse(
        workflow_id: "echo-example",
        run_id: "run-example",
      ))
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
