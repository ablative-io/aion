//// WorkflowHandle and per-workflow operations.

import aion_client as client_mod
import aion_client/error.{type Error}
import aion_client/payload as payload_mod
import aion_client/stream as stream_mod
import gleam/dynamic/decode
import gleam/json
import gleam/option.{type Option, None, Some}

pub fn new(
  client: client_mod.Client,
  workflow_id: String,
  run_id: String,
) -> client_mod.WorkflowHandle {
  client_mod.WorkflowHandle(
    client: client,
    workflow_id: workflow_id,
    run_id: run_id,
  )
}

pub fn signal(
  handle: client_mod.WorkflowHandle,
  signal_name: String,
  input: input,
  encoder: fn(input) -> json.Json,
) -> Result(Nil, Error) {
  signal_raw(handle, signal_name, payload_mod.encode(input, encoder))
}

pub fn signal_raw(
  handle: client_mod.WorkflowHandle,
  signal_name: String,
  input: payload_mod.Payload,
) -> Result(Nil, Error) {
  let client_mod.WorkflowHandle(
    client: client,
    workflow_id: workflow_id,
    run_id: run_id,
  ) = handle
  client_mod.signal_raw(
    client,
    client_mod.SignalOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      signal_name: signal_name,
    ),
    input,
  )
}

pub fn query(
  handle: client_mod.WorkflowHandle,
  query_name: String,
  args: args,
  encoder: fn(args) -> json.Json,
  decoder: decode.Decoder(result),
) -> Result(result, Error) {
  query_raw(handle, query_name, payload_mod.encode(args, encoder), decoder)
}

pub fn query_raw(
  handle: client_mod.WorkflowHandle,
  query_name: String,
  args: payload_mod.Payload,
  decoder: decode.Decoder(result),
) -> Result(result, Error) {
  let client_mod.WorkflowHandle(
    client: client,
    workflow_id: workflow_id,
    run_id: run_id,
  ) = handle
  client_mod.query_raw(
    client,
    client_mod.QueryOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      query_name: query_name,
    ),
    args,
    decoder,
  )
}

pub fn query_payload(
  handle: client_mod.WorkflowHandle,
  query_name: String,
  args: payload_mod.Payload,
) -> Result(payload_mod.Payload, Error) {
  let client_mod.WorkflowHandle(
    client: client,
    workflow_id: workflow_id,
    run_id: run_id,
  ) = handle
  client_mod.query_payload(
    client,
    client_mod.QueryOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      query_name: query_name,
    ),
    args,
  )
}

pub fn cancel(
  handle: client_mod.WorkflowHandle,
  reason: String,
) -> Result(Nil, Error) {
  let client_mod.WorkflowHandle(
    client: client,
    workflow_id: workflow_id,
    run_id: run_id,
  ) = handle
  client_mod.cancel(
    client,
    client_mod.CancelOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      reason: reason,
    ),
  )
}

pub fn describe(
  handle: client_mod.WorkflowHandle,
) -> Result(client_mod.WorkflowDescription, Error) {
  let client_mod.WorkflowHandle(
    client: client,
    workflow_id: workflow_id,
    run_id: run_id,
  ) = handle
  client_mod.describe(
    client,
    client_mod.DescribeOptions(workflow_id: workflow_id, run_id: Some(run_id)),
  )
}

pub fn subscribe(
  handle: client_mod.WorkflowHandle,
  decoder: decode.Decoder(event),
) -> stream_mod.EventStream(event) {
  stream_mod.subscribe(handle, decoder)
}

pub fn with_run_id(
  handle: client_mod.WorkflowHandle,
  run_id: String,
) -> client_mod.WorkflowHandle {
  let client_mod.WorkflowHandle(client: client, workflow_id: workflow_id, ..) =
    handle
  client_mod.WorkflowHandle(
    client: client,
    workflow_id: workflow_id,
    run_id: run_id,
  )
}

pub fn run_id(handle: client_mod.WorkflowHandle) -> String {
  let client_mod.WorkflowHandle(run_id: run_id, ..) = handle
  run_id
}

pub fn workflow_id(handle: client_mod.WorkflowHandle) -> String {
  let client_mod.WorkflowHandle(workflow_id: workflow_id, ..) = handle
  workflow_id
}

pub fn target_run(
  run_id: Option(String),
  handle: client_mod.WorkflowHandle,
) -> client_mod.WorkflowHandle {
  case run_id {
    Some(run_id) -> with_run_id(handle, run_id)
    None -> handle
  }
}
