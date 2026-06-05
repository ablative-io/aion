//// workflow.define: typed entry contract (entry fn + input/output/error codecs)

import aion/codec.{type Codec}

/// A typed workflow entry contract for the Aion engine.
///
/// Workflow authors write a plain function `fn(input) -> Result(output, error)`
/// and expose it to the engine by returning a `WorkflowDefinition` from the
/// package entry function named in the `.aion` manifest. AE consumes this value
/// at spawn: it calls that named package entry function, receives the definition,
/// decodes the input Payload with `input_codec`, invokes/drives the typed entry
/// function, and encodes the output or error Payload with `output_codec` or
/// `error_codec`.
///
/// AP (`aion-package`) records only the entry module and function names as
/// strings in the manifest; it does not introspect this Gleam value. The SDK
/// carries the contract but does not invoke `entry_fn` itself.
pub opaque type WorkflowDefinition(input, output, error) {
  WorkflowDefinition(
    name: String,
    input_codec: Codec(input),
    output_codec: Codec(output),
    error_codec: Codec(error),
    entry_fn: fn(input) -> Result(output, error),
  )
}

/// Define a typed workflow entry contract consumed by AE at spawn.
pub fn define(
  name: String,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(error),
  entry_fn: fn(input) -> Result(output, error),
) -> WorkflowDefinition(input, output, error) {
  WorkflowDefinition(
    name: name,
    input_codec: input_codec,
    output_codec: output_codec,
    error_codec: error_codec,
    entry_fn: entry_fn,
  )
}

/// Return the workflow name carried by the definition.
pub fn name(definition: WorkflowDefinition(input, output, error)) -> String {
  definition.name
}

/// Return the input codec used by AE to decode the spawn Payload.
pub fn input_codec(
  definition: WorkflowDefinition(input, output, error),
) -> Codec(input) {
  definition.input_codec
}

/// Return the output codec used by AE to encode successful completion Payloads.
pub fn output_codec(
  definition: WorkflowDefinition(input, output, error),
) -> Codec(output) {
  definition.output_codec
}

/// Return the error codec used by AE to encode failed completion Payloads.
pub fn error_codec(
  definition: WorkflowDefinition(input, output, error),
) -> Codec(error) {
  definition.error_codec
}

/// Return the typed entry function carried for AE to invoke/drive.
pub fn entry_fn(
  definition: WorkflowDefinition(input, output, error),
) -> fn(input) -> Result(output, error) {
  definition.entry_fn
}
