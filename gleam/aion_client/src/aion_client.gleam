//// Caller-side SDK for aion-server workflow operations.

import aion_client/error.{type Error}
import aion_client/payload
import aion_client/payload.{type Payload}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}

pub type Config {
  Config(
    endpoint: String,
    bearer_token: Option(String),
    namespace: String,
    tls: Bool,
  )
}

pub type Client {
  Client(config: Config, transport: Transport)
}

pub type WorkflowHandle {
  WorkflowHandle(client: Client, workflow_id: String, run_id: String)
}

pub type Transport {
  Transport(
    start: fn(Config, StartRequest) -> Result(StartResponse, Error),
    signal: fn(Config, SignalRequest) -> Result(Nil, Error),
    query: fn(Config, QueryRequest) -> Result(Payload, Error),
    cancel: fn(Config, CancelRequest) -> Result(Nil, Error),
    list: fn(Config, ListRequest) -> Result(List(WorkflowSummary), Error),
    describe: fn(Config, DescribeRequest) -> Result(WorkflowDescription, Error),
  )
}

pub type StartOptions {
  StartOptions(
    workflow_id: String,
    workflow_type: String,
    task_queue: String,
    idempotency_key: Option(String),
  )
}

pub type SignalOptions {
  SignalOptions(workflow_id: String, run_id: Option(String), signal_name: String)
}

pub type QueryOptions {
  QueryOptions(workflow_id: String, run_id: Option(String), query_name: String)
}

pub type CancelOptions {
  CancelOptions(workflow_id: String, run_id: Option(String), reason: String)
}

pub type ListOptions {
  ListOptions(namespace: Option(String))
}

pub type DescribeOptions {
  DescribeOptions(workflow_id: String, run_id: Option(String))
}

pub type StartRequest {
  StartRequest(options: StartOptions, input: Payload)
}

pub type StartResponse {
  StartResponse(workflow_id: String, run_id: String)
}

pub type SignalRequest {
  SignalRequest(options: SignalOptions, input: Payload)
}

pub type QueryRequest {
  QueryRequest(options: QueryOptions, args: Payload)
}

pub type CancelRequest {
  CancelRequest(options: CancelOptions)
}

pub type ListRequest {
  ListRequest(options: ListOptions)
}

pub type DescribeRequest {
  DescribeRequest(options: DescribeOptions)
}

pub type WorkflowSummary {
  WorkflowSummary(workflow_id: String, run_id: String, workflow_type: String, status: String)
}

pub type WorkflowDescription {
  WorkflowDescription(
    workflow_id: String,
    run_id: String,
    workflow_type: String,
    status: String,
  )
}

/// Connect once to an aion-server deployment. The current implementation stores
/// validated connection configuration and reuses it for all operations; concrete
/// transport failures surface as operation Results, preserving branchable errors.
pub fn connect(config: Config) -> Result(Client, Error) {
  case config.endpoint == "" || config.namespace == "" {
    True -> Error(error.InvalidArgument)
    False -> Ok(Client(config: config, transport: unavailable_transport()))
  }
}

/// Test/conformance hook for injecting an HTTP/WebSocket transport while keeping
/// the public SDK semantics identical.
pub fn with_transport(config: Config, transport: Transport) -> Result(Client, Error) {
  case config.endpoint == "" || config.namespace == "" {
    True -> Error(error.InvalidArgument)
    False -> Ok(Client(config: config, transport: transport))
  }
}

pub fn start(
  client: Client,
  options: StartOptions,
  input: input,
  encoder: fn(input) -> json.Json,
) -> Result(WorkflowHandle, Error) {
  start_raw(client, options, payload.encode(input, encoder))
}

pub fn start_raw(
  client: Client,
  options: StartOptions,
  input: Payload,
) -> Result(WorkflowHandle, Error) {
  let Client(config: config, transport: transport) = client

  case transport.start(config, StartRequest(options: options, input: input)) {
    Ok(StartResponse(workflow_id: workflow_id, run_id: run_id)) ->
      Ok(WorkflowHandle(client: client, workflow_id: workflow_id, run_id: run_id))
    Error(error) -> Error(error)
  }
}

pub fn signal(
  client: Client,
  options: SignalOptions,
  input: input,
  encoder: fn(input) -> json.Json,
) -> Result(Nil, Error) {
  signal_raw(client, options, payload.encode(input, encoder))
}

pub fn signal_raw(
  client: Client,
  options: SignalOptions,
  input: Payload,
) -> Result(Nil, Error) {
  let Client(config: config, transport: transport) = client
  transport.signal(config, SignalRequest(options: options, input: input))
}

pub fn query(
  client: Client,
  options: QueryOptions,
  args: args,
  encoder: fn(args) -> json.Json,
  decoder: decode.Decoder(result),
) -> Result(result, Error) {
  query_raw(client, options, payload.encode(args, encoder), decoder)
}

pub fn query_raw(
  client: Client,
  options: QueryOptions,
  args: Payload,
  decoder: decode.Decoder(result),
) -> Result(result, Error) {
  case query_payload(client, options, args) {
    Ok(reply) -> payload.decode(reply, decoder)
    Error(error) -> Error(error)
  }
}

pub fn query_payload(
  client: Client,
  options: QueryOptions,
  args: Payload,
) -> Result(Payload, Error) {
  let Client(config: config, transport: transport) = client
  transport.query(config, QueryRequest(options: options, args: args))
}

pub fn cancel(client: Client, options: CancelOptions) -> Result(Nil, Error) {
  let Client(config: config, transport: transport) = client
  transport.cancel(config, CancelRequest(options: options))
}

pub fn list(client: Client, options: ListOptions) -> Result(List(WorkflowSummary), Error) {
  let Client(config: config, transport: transport) = client
  transport.list(config, ListRequest(options: options))
}

pub fn describe(
  client: Client,
  options: DescribeOptions,
) -> Result(WorkflowDescription, Error) {
  let Client(config: config, transport: transport) = client
  transport.describe(config, DescribeRequest(options: options))
}

pub fn latest_run(run_id: Option(String), default_run_id: String) -> String {
  case run_id {
    Some(run_id) -> run_id
    None -> default_run_id
  }
}

pub fn default_list_options(config: Config) -> ListOptions {
  let Config(namespace: namespace, ..) = config
  ListOptions(namespace: Some(namespace))
}

pub fn workflow_ids(summaries: List(WorkflowSummary)) -> List(String) {
  summaries
  |> list.map(fn(summary) {
    let WorkflowSummary(workflow_id: workflow_id, ..) = summary
    workflow_id
  })
}

fn unavailable_transport() -> Transport {
  Transport(
    start: fn(_, _) { Error(error.Unavailable) },
    signal: fn(_, _) { Error(error.Unavailable) },
    query: fn(_, _) { Error(error.Unavailable) },
    cancel: fn(_, _) { Error(error.Unavailable) },
    list: fn(_, _) { Error(error.Unavailable) },
    describe: fn(_, _) { Error(error.Unavailable) },
  )
}
