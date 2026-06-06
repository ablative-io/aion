//// Live conformance harness entry point for the shared aion client scenarios.
////
//// Gleam's current SDK surface has an injectable transport but no checked-in live
//// HTTP/WebSocket transport. This harness still owns the same runtime env gate,
//// reads the shared scenario document through the companion FFI, and reports the
//// SDK divergence explicitly as the shared `Unavailable` taxonomy variant when a
//// live server URL is supplied.

import aion_client
import aion_client/error
import gleam/io
import gleam/list
import gleam/result
import gleam/string
import gleeunit
import gleeunit/should

const scenarios_path = "../../../conformance/aion-clients/scenarios.json"
const server_url_env = "AION_SERVER_URL"

pub fn main() {
  gleeunit.main()
}

pub fn shared_client_contract_conformance_test() {
  case getenv(server_url_env) {
    "" -> {
      io.println(
        "SKIP sdk=gleam reason=" <> server_url_env <> " is unset; live aion-server conformance not run",
      )
      should.equal(Ok(Nil), Ok(Nil))
    }
    server_url -> {
      let assert Ok(source) = read_file(scenarios_path)
      let scenario_ids = extract_ids(source, "\"id\": ")
      let config =
        aion_client.Config(
          endpoint: server_url,
          bearer_token: None,
          namespace: "conformance",
          tls: string.starts_with(server_url, "https://"),
        )
      let client_result = aion_client.connect(config)
      scenario_ids
      |> list.each(fn(scenario_id) {
        let actual = normalised_result(client_result)
        io.println(
          "AION_CONFORMANCE sdk=gleam scenario=" <> scenario_id <> " step=connect result=" <> actual,
        )
        should.equal(actual, "{\"ok\":{\"kind\":\"client\"}}")
      })
    }
  }
}

fn normalised_result(result: Result(aion_client.Client, error.Error)) -> String {
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

fn extract_ids(source: String, marker: String) -> List(String) {
  source
  |> string.split(marker)
  |> list.drop(1)
  |> list.filter_map(first_quoted)
}

fn first_quoted(chunk: String) -> Result(String, Nil) {
  case string.split(chunk, "\"") {
    [_, id, ..] -> Ok(id)
    _ -> Error(Nil)
  }
}

@external(erlang, "aion_client_conformance_ffi", "getenv")
fn getenv(name: String) -> String

@external(erlang, "aion_client_conformance_ffi", "read_file")
fn read_file(path: String) -> Result(String, String)
