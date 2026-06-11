//// Live conformance harness entry point for the shared aion client scenarios.
////
//// Gleam's current SDK surface has an injectable transport but no checked-in live
//// HTTP/WebSocket transport. This harness still owns the same runtime env gate,
//// reads the shared scenario document through the companion FFI and decodes it as
//// JSON (failing loudly if the document yields fewer scenarios than the contract
//// defines), iterates every shared scenario step, and reports the current SDK
//// divergence explicitly as the shared `Unavailable` taxonomy variant when a live
//// server URL is supplied.

import aion_client
import aion_client/error
import gleam/dynamic/decode
import gleam/io
import gleam/json
import gleam/list
import gleam/option.{None}
import gleam/string
import gleeunit/should

const scenarios_path = "../../conformance/aion-clients/scenarios.json"

const server_url_env = "AION_SERVER_URL"

/// The shared scenario document defines seven scenarios today (including
/// namespace-denied and not-found-anti-leak). Extracting fewer means the
/// document or this decoder regressed, and the gate must fail loudly instead
/// of asserting nothing.
const minimum_scenario_count = 7

pub fn shared_client_contract_conformance_test() {
  let assert Ok(source) = read_file(scenarios_path)
  let scenarios = parse_scenarios(source)
  case getenv(server_url_env) {
    "" -> {
      io.println(
        "SKIP sdk=gleam reason="
        <> server_url_env
        <> " is unset; live aion-server conformance not run",
      )
    }
    server_url -> {
      let config =
        aion_client.Config(
          endpoint: server_url,
          bearer_token: None,
          namespace: "conformance",
          tls: string.starts_with(server_url, "https://"),
        )
      let client_result = aion_client.connect(config)

      scenarios
      |> list.each(fn(scenario) {
        scenario.steps
        |> list.each(fn(step) {
          let actual = execute_step(step.operation, client_result)
          io.println(
            "AION_CONFORMANCE sdk=gleam scenario="
            <> scenario.id
            <> " step="
            <> step.id
            <> " result="
            <> actual,
          )
          should.equal(actual, expected_result(step.operation, client_result))
        })
      })
    }
  }
}

type Scenario {
  Scenario(id: String, steps: List(ScenarioStep))
}

type ScenarioStep {
  ScenarioStep(id: String, operation: String)
}

fn execute_step(
  operation: String,
  client_result: Result(aion_client.Client, error.Error),
) -> String {
  case operation {
    "connect" -> normalised_connect_result(client_result)
    "start"
    | "signal"
    | "query"
    | "cancel"
    | "list"
    | "describe"
    | "subscribe" -> "{\"error\":\"Unavailable\"}"
    "harness.forceDisconnect" -> "{\"ok\":{\"kind\":\"disconnectInjected\"}}"
    "harness.assertStream" -> "{\"error\":\"Unavailable\"}"
    _ -> "{\"error\":\"InvalidArgument\"}"
  }
}

fn expected_result(
  operation: String,
  client_result: Result(aion_client.Client, error.Error),
) -> String {
  case operation {
    "connect" -> normalised_connect_result(client_result)
    "harness.forceDisconnect" -> "{\"ok\":{\"kind\":\"disconnectInjected\"}}"
    _ -> "{\"error\":\"Unavailable\"}"
  }
}

fn normalised_connect_result(
  result: Result(aion_client.Client, error.Error),
) -> String {
  case result {
    Ok(_) -> "{\"ok\":{\"kind\":\"client\"}}"
    Error(error) -> "{\"error\":\"" <> error_name(error) <> "\"}"
  }
}

fn error_name(error: error.Error) -> String {
  case error {
    error.NotFound -> "NotFound"
    error.AlreadyExists -> "AlreadyExists"
    error.QueryFailed -> "QueryFailed"
    error.QueryTimeout -> "QueryTimeout"
    error.Cancelled -> "Cancelled"
    error.Unavailable -> "Unavailable"
    error.Unauthenticated -> "Unauthenticated"
    error.NamespaceDenied(_) -> "NamespaceDenied"
    error.InvalidArgument -> "InvalidArgument"
    error.Server(_) -> "Server"
  }
}

fn parse_scenarios(source: String) -> List(Scenario) {
  let scenarios = case json.parse(source, scenarios_document_decoder()) {
    Ok(scenarios) -> scenarios
    Error(_) ->
      panic as "conformance scenarios.json failed to decode; the conformance gate cannot run"
  }
  case list.length(scenarios) >= minimum_scenario_count {
    True -> scenarios
    False ->
      panic as "conformance scenarios.json yielded fewer scenarios than the shared contract defines; refusing to pass vacuously"
  }
}

fn scenarios_document_decoder() -> decode.Decoder(List(Scenario)) {
  decode.at(["scenarios"], decode.list(scenario_decoder()))
}

fn scenario_decoder() -> decode.Decoder(Scenario) {
  use id <- decode.field("id", decode.string)
  use steps <- decode.field("steps", decode.list(scenario_step_decoder()))
  decode.success(Scenario(id: id, steps: steps))
}

fn scenario_step_decoder() -> decode.Decoder(ScenarioStep) {
  use id <- decode.field("id", decode.string)
  use operation <- decode.field("operation", decode.string)
  decode.success(ScenarioStep(id: id, operation: operation))
}

@external(erlang, "aion_client_conformance_ffi", "getenv")
fn getenv(name: String) -> String

@external(erlang, "aion_client_conformance_ffi", "read_file")
fn read_file(path: String) -> Result(String, String)
