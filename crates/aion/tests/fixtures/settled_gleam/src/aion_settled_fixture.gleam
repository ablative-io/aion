import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

fn string_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn gated_activity(key: String) -> activity.Activity(String, String) {
  let name = case key {
    "b" -> "gated_fail:b"
    other -> "gated_ok:" <> other
  }
  activity.new(name, "in", string_codec(), string_codec(), fn(_) {
    Error(error.ActivityEngineFailure("remote fixture runner was called"))
  })
}

fn settled_text(slots: List(Result(String, error.ActivityError))) -> String {
  case slots {
    [Ok(a), Error(_), Ok(c)] -> "ok=" <> a <> "|err=boom-b|ok=" <> c
    _ -> "unexpected settled slots"
  }
}

/// Engine entry exercising the public settled fan-out API.
pub fn settled_three(_input: Dynamic) -> String {
  ["a", "b", "c"]
  |> workflow.map_settled(gated_activity)
  |> settled_text
  |> json.string
  |> json.to_string
}
