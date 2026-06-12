//// {{name}} — minimal Aion durable workflow (hello-world template).
////
//// Decodes a typed input, returns a typed output, and completes — no
//// activities, signals, or timers. Edit `handle` below; the raw engine
//// plumbing lives under the generated-code marker at the bottom.

import aion/codec
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

pub type HelloInput {
  HelloInput(name: String)
}

pub type HelloOutput {
  HelloOutput(greeting: String)
}

pub type WorkflowError {
  InvalidInput(message: String)
}

/// Your typed workflow. The engine plumbing below decodes the start input
/// into a `HelloInput`, calls this function, and records the encoded
/// `HelloOutput` as the workflow result.
pub fn handle(input: HelloInput) -> Result(HelloOutput, WorkflowError) {
  Ok(HelloOutput(greeting: "Hello, " <> input.name <> "!"))
}

// ---------------------------------------------------------------------------
// Generated plumbing — written by `aion new`. You normally never edit this.
//
// `run` is the engine entry point named by `workflow.toml`. The runtime
// delivers the start input as a raw JSON string inside a `Dynamic`: decode
// it, parse it with the input codec, run the typed `handle`, and encode the
// success value back to a JSON string for the recorded result payload. The
// codecs mirror the JSON Schemas in `schemas/`.
// ---------------------------------------------------------------------------

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case input_codec().decode(raw_json) {
        Ok(input) ->
          case handle(input) {
            Ok(output) -> Ok(output_codec().encode(output))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(InvalidInput("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(InvalidInput("workflow input payload was not a string"))
  }
}

fn input_codec() -> codec.Codec(HelloInput) {
  codec.json_codec(hello_input_to_json, hello_input_decoder())
}

fn hello_input_to_json(input: HelloInput) -> json.Json {
  json.object([#("name", json.string(input.name))])
}

fn hello_input_decoder() -> decode.Decoder(HelloInput) {
  use name <- decode.field("name", decode.string)
  decode.success(HelloInput(name: name))
}

fn output_codec() -> codec.Codec(HelloOutput) {
  codec.json_codec(hello_output_to_json, hello_output_decoder())
}

fn hello_output_to_json(output: HelloOutput) -> json.Json {
  json.object([#("greeting", json.string(output.greeting))])
}

fn hello_output_decoder() -> decode.Decoder(HelloOutput) {
  use greeting <- decode.field("greeting", decode.string)
  decode.success(HelloOutput(greeting: greeting))
}
