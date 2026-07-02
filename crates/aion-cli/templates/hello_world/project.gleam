//// {{name}} — minimal Aion durable workflow (hello-world template).
////
//// Decodes a typed input, returns a typed output, and completes — no
//// activities, signals, or timers. Edit `handle` below; the raw engine
//// plumbing lives under the generated-code marker at the bottom. The
//// boundary types are authored in `src/{{name}}_io.gleam`; their codecs
//// (`src/{{name}}_codecs.gleam`) and the `schemas/*.json` artifacts are
//// generated from those types by `aion generate` (ADR-014, types-first).

import aion/codec
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import {{name}}_codecs as codecs
import {{name}}_io as io

pub type WorkflowError {
  InvalidInput(message: String)
}

/// Your typed workflow. The engine plumbing below decodes the start input
/// into an `io.Input`, calls this function, and records the encoded
/// `io.Output` as the workflow result.
pub fn handle(input: io.Input) -> Result(io.Output, WorkflowError) {
  Ok(io.Output(greeting: "Hello, " <> input.name <> "!"))
}

// ---------------------------------------------------------------------------
// Generated plumbing — written by `aion new`. You normally never edit this.
//
// `run` is the engine entry point named by `workflow.toml`. The runtime
// delivers the start input as a raw JSON string inside a `Dynamic`: decode
// it, parse it with the generated input codec, run the typed `handle`, and
// encode the success value back to a JSON string for the recorded result
// payload. The codecs are generated from the types in `{{name}}_io`.
// ---------------------------------------------------------------------------

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.input_codec().decode(raw_json) {
        Ok(input) ->
          case handle(input) {
            Ok(output) -> Ok(codecs.output_codec().encode(output))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(InvalidInput("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(InvalidInput("workflow input payload was not a string"))
  }
}
