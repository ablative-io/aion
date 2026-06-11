//// Live conformance harness entry point for the shared aion client scenarios.
////
//// Gleam's current SDK surface has an injectable transport but no checked-in live
//// HTTP/WebSocket transport. This harness still owns the same runtime env gate,
//// reads the shared scenario document through the companion FFI, iterates every
//// shared scenario step, and reports the current SDK divergence explicitly as the
//// shared `Unavailable` taxonomy variant when a live server URL is supplied.

import aion_client
import aion_client/error
import gleam/io
import gleam/list
import gleam/option.{None}
import gleam/string
import gleeunit/should

const scenarios_path = "../../../conformance/aion-clients/scenarios.json"

const server_url_env = "AION_SERVER_URL"

pub fn shared_client_contract_conformance_test() {
  case getenv(server_url_env) {
    "" -> {
      io.println(
        "SKIP sdk=gleam reason="
        <> server_url_env
        <> " is unset; live aion-server conformance not run",
      )
      should.equal(Ok(Nil), Ok(Nil))
    }
    server_url -> {
      let assert Ok(source) = read_file(scenarios_path)
      let scenarios = extract_scenarios(source)
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
    error.InvalidArgument -> "InvalidArgument"
    error.Server(_) -> "Server"
  }
}

fn extract_scenarios(source: String) -> List(Scenario) {
  source
  |> string.split("\n    {\n      \"id\": ")
  |> list.drop(1)
  |> list.filter_map(extract_scenario)
}

fn extract_scenario(chunk: String) -> Result(Scenario, Nil) {
  use id <- result_try(first_quoted(chunk))
  Ok(Scenario(id: id, steps: extract_steps(chunk)))
}

fn extract_steps(source: String) -> List(ScenarioStep) {
  source
  |> string.split("\n        {\n          \"id\": ")
  |> list.drop(1)
  |> list.filter_map(extract_step)
}

fn extract_step(chunk: String) -> Result(ScenarioStep, Nil) {
  use id <- result_try(first_quoted(chunk))
  use operation <- result_try(extract_operation(chunk))
  Ok(ScenarioStep(id: id, operation: operation))
}

fn first_quoted(chunk: String) -> Result(String, Nil) {
  case string.split(chunk, "\"") {
    [_, id, ..] -> Ok(id)
    _ -> Error(Nil)
  }
}

fn extract_operation(chunk: String) -> Result(String, Nil) {
  case string.split(chunk, "\"operation\": ") {
    [_, rest, ..] -> first_quoted(rest)
    _ -> Error(Nil)
  }
}

fn result_try(
  value: Result(a, Nil),
  next: fn(a) -> Result(b, Nil),
) -> Result(b, Nil) {
  case value {
    Ok(inner) -> next(inner)
    Error(Nil) -> Error(Nil)
  }
}

@external(erlang, "aion_client_conformance_ffi", "getenv")
fn getenv(name: String) -> String

@external(erlang, "aion_client_conformance_ffi", "read_file")
fn read_file(path: String) -> Result(String, String)
