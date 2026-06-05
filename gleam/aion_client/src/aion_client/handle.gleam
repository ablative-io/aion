//// WorkflowHandle and per-workflow operations.

import aion_client
import aion_client.{type Client, type WorkflowDescription, type WorkflowHandle}
import aion_client/error.{type Error}
import aion_client/payload
import aion_client/payload.{type Payload}
import aion_client/stream
import aion_client/stream.{type EventStream}
import gleam/dynamic/decode
import gleam/json
import gleam/option.{type Option, None, Some}

pub fn new(client: Client, workflow_id: String, run_id: String) -> WorkflowHandle {
  aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id)
}

pub fn signal(
  handle: WorkflowHandle,
  signal_name: String,
  input: input,
  encoder: fn(input) -> json.Json,
) -> Result(Nil, Error) {
  signal_raw(handle, signal_name, payload.encode(input, encoder))
}

pub fn signal_raw(
  handle: WorkflowHandle,
  signal_name: String,
  input: Payload,
) -> Result(Nil, Error) {
  let aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id) =
    handle
  aion_client.signal_raw(
    client,
    aion_client.SignalOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      signal_name: signal_name,
    ),
    input,
  )
}

pub fn query(
  handle: WorkflowHandle,
  query_name: String,
  args: args,
  encoder: fn(args) -> json.Json,
  decoder: decode.Decoder(result),
) -> Result(result, Error) {
  query_raw(handle, query_name, payload.encode(args, encoder), decoder)
}

pub fn query_raw(
  handle: WorkflowHandle,
  query_name: String,
  args: Payload,
  decoder: decode.Decoder(result),
) -> Result(result, Error) {
  let aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id) =
    handle
  aion_client.query_raw(
    client,
    aion_client.QueryOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      query_name: query_name,
    ),
    args,
    decoder,
  )
}

pub fn query_payload(
  handle: WorkflowHandle,
  query_name: String,
  args: Payload,
) -> Result(Payload, Error) {
  let aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id) =
    handle
  aion_client.query_payload(
    client,
    aion_client.QueryOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      query_name: query_name,
    ),
    args,
  )
}

pub fn cancel(handle: WorkflowHandle, reason: String) -> Result(Nil, Error) {
  let aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id) =
    handle
  aion_client.cancel(
    client,
    aion_client.CancelOptions(
      workflow_id: workflow_id,
      run_id: Some(run_id),
      reason: reason,
    ),
  )
}

pub fn describe(handle: WorkflowHandle) -> Result(WorkflowDescription, Error) {
  let aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id) =
    handle
  aion_client.describe(
    client,
    aion_client.DescribeOptions(workflow_id: workflow_id, run_id: Some(run_id)),
  )
}

pub fn subscribe(
  handle: WorkflowHandle,
  decoder: decode.Decoder(event),
) -> EventStream(event) {
  stream.subscribe(handle, decoder)
}

pub fn with_run_id(handle: WorkflowHandle, run_id: String) -> WorkflowHandle {
  let aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, ..) =
    handle
  aion_client.WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id)
}

pub fn run_id(handle: WorkflowHandle) -> String {
  let aion_client.WorkflowHandle(run_id: run_id, ..) = handle
  run_id
}

pub fn workflow_id(handle: WorkflowHandle) -> String {
  let aion_client.WorkflowHandle(workflow_id: workflow_id, ..) = handle
  workflow_id
}

pub fn target_run(run_id: Option(String), handle: WorkflowHandle) -> WorkflowHandle {
  case run_id {
    Some(run_id) -> with_run_id(handle, run_id)
    None -> handle
  }
}
