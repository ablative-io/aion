//// JSON codecs for the workflow-level inputs, outputs, typed errors, and
//// status replies of the three workflow entries.
////
//// Every input/output codec here is built ON the schema-generated module
//// (`{{name}}_io`, regenerated with `aion codegen`): the generated
//// encoder/decoder owns the wire shape, and `{{name}}/io_convert`
//// maps it to the domain types the workflow bodies use — so the schemas in
//// `schemas/` are the single source of truth for workflow I/O, and schema
//// drift is a compile error or a `--check` failure, never silent. The typed
//// workflow ERRORS and the live STATUS replies have no schemas (they are
//// not dispatch-boundary payloads), so their codecs stay hand-written here.
////
//// Activity-payload codecs live in `{{name}}/codecs_core` (workspace,
//// startup, dev, checks) and `{{name}}/codecs_flow` (review, land).

import {{name}}/io_convert
import {{name}}/types.{
  type DevFlowError, type DevFlowInput, type DevFlowResult, type DevFlowStatus,
  type GateError, type GateInput, type GateResult, type PipelineError,
  type PipelineInput, type PipelineResult, type PipelineStatus, DevFailed,
  DevFlowStageFailed, DevFlowStatus, GateInput, GatePass, GateRejected,
  GateResult, GateStageFailed, LandFailed, Local, PipelineStatus,
  ProvisionFailed, ReviewCapExhausted, ReviewRejected, ReviewTimedOut,
  StageFailed, StartupFailed, VerifyExhausted, VerifyFixExhausted, Workspace,
  WorkspaceWide, Worktree,
}
import {{name}}_io as generated
import aion/codec
import gleam/dynamic/decode
import gleam/json

/// Codec for the top-level pipeline workflow input
/// (`schemas/input.json`, generated decode/encode).
pub fn pipeline_input_codec() -> codec.Codec(PipelineInput) {
  codec.json_codec(
    fn(input: PipelineInput) {
      generated.input_to_json(io_convert.input_from_domain(input))
    },
    decode.map(generated.input_decoder(), io_convert.input_to_domain),
  )
}

/// Codec for the top-level pipeline workflow output
/// (`schemas/output.json`, generated decode/encode).
pub fn pipeline_result_codec() -> codec.Codec(PipelineResult) {
  codec.json_codec(
    fn(result: PipelineResult) {
      generated.output_to_json(io_convert.output_from_domain(result))
    },
    decode.map(generated.output_decoder(), io_convert.output_to_domain),
  )
}

/// Codec for the dev child workflow input
/// (`schemas/dev_input.json`, generated decode/encode).
pub fn dev_flow_input_codec() -> codec.Codec(DevFlowInput) {
  codec.json_codec(
    fn(input: DevFlowInput) {
      generated.dev_input_to_json(io_convert.dev_input_from_domain(input))
    },
    decode.map(generated.dev_input_decoder(), io_convert.dev_input_to_domain),
  )
}

/// Codec for the dev child workflow output
/// (`schemas/dev_output.json`, generated decode/encode).
pub fn dev_flow_result_codec() -> codec.Codec(DevFlowResult) {
  codec.json_codec(
    fn(result: DevFlowResult) {
      generated.dev_output_to_json(io_convert.dev_output_from_domain(result))
    },
    decode.map(generated.dev_output_decoder(), io_convert.dev_output_to_domain),
  )
}

/// Codec for the gate child workflow input
/// (`schemas/gate_input.json`, generated decode/encode). The schema cannot
/// state that `modules` is required exactly when the scope kind is
/// `affected_closure`; `io_convert` enforces it, and a violation is a
/// decode failure here.
pub fn gate_input_codec() -> codec.Codec(GateInput) {
  codec.json_codec(
    fn(input: GateInput) {
      generated.gate_input_to_json(io_convert.gate_input_from_domain(input))
    },
    decode.then(generated.gate_input_decoder(), fn(wire) {
      case io_convert.gate_input_to_domain(wire) {
        Ok(input) -> decode.success(input)
        Error(reason) -> decode.failure(fallback_gate_input(), reason)
      }
    }),
  )
}

/// Codec for the gate child workflow output — also the `full_checks`
/// activity output (`schemas/gate_output.json`, generated decode/encode).
pub fn gate_result_codec() -> codec.Codec(GateResult) {
  codec.json_codec(gate_result_to_json, gate_result_decoder())
}

/// JSON encoder for a `GateResult`, shared with the `request_review`
/// activity codec in `{{name}}/codecs_flow`, which embeds the gate
/// result.
pub fn gate_result_to_json(result: GateResult) -> json.Json {
  generated.gate_output_to_json(io_convert.gate_output_from_domain(result))
}

/// Decoder for a `GateResult`, shared with the `request_review` activity
/// codec. The schema cannot state that `report` is required exactly when
/// the outcome is `fail`; `io_convert` enforces it, and a violation is a
/// decode failure here.
pub fn gate_result_decoder() -> decode.Decoder(GateResult) {
  decode.then(generated.gate_output_decoder(), fn(wire) {
    case io_convert.gate_output_to_domain(wire) {
      Ok(result) -> decode.success(result)
      Error(reason) -> decode.failure(GateResult(verdict: GatePass), reason)
    }
  })
}

/// Codec for the `gate` child's typed error. Workflow errors are not
/// dispatch-boundary payloads, so this codec has no schema and stays
/// hand-written.
pub fn gate_error_codec() -> codec.Codec(GateError) {
  codec.json_codec(
    fn(gate_error: GateError) {
      case gate_error {
        GateStageFailed(stage: stage, message: message) ->
          json.object([
            #("stage", json.string(stage)),
            #("message", json.string(message)),
          ])
      }
    },
    {
      use stage <- decode.field("stage", decode.string)
      use message <- decode.field("message", decode.string)
      decode.success(GateStageFailed(stage: stage, message: message))
    },
  )
}

/// Codec for the dev child workflow's typed error (no schema — errors are
/// not dispatch-boundary payloads).
pub fn dev_flow_error_codec() -> codec.Codec(DevFlowError) {
  codec.json_codec(
    fn(dev_flow_error: DevFlowError) {
      case dev_flow_error {
        StartupFailed(message: message) ->
          json.object([
            #("error", json.string("startup_failed")),
            #("message", json.string(message)),
          ])
        VerifyFixExhausted(rounds: rounds, diagnostics: diagnostics) ->
          json.object([
            #("error", json.string("verify_fix_exhausted")),
            #("rounds", json.int(rounds)),
            #("diagnostics", json.string(diagnostics)),
          ])
        DevFlowStageFailed(stage: stage, message: message) ->
          json.object([
            #("error", json.string("stage_failed")),
            #("stage", json.string(stage)),
            #("message", json.string(message)),
          ])
      }
    },
    {
      use tag <- decode.field("error", decode.string)
      case tag {
        "startup_failed" -> {
          use message <- decode.field("message", decode.string)
          decode.success(StartupFailed(message: message))
        }
        "verify_fix_exhausted" -> {
          use rounds <- decode.field("rounds", decode.int)
          use diagnostics <- decode.field("diagnostics", decode.string)
          decode.success(VerifyFixExhausted(
            rounds: rounds,
            diagnostics: diagnostics,
          ))
        }
        "stage_failed" -> {
          use stage <- decode.field("stage", decode.string)
          use message <- decode.field("message", decode.string)
          decode.success(DevFlowStageFailed(stage: stage, message: message))
        }
        _ ->
          decode.failure(
            StartupFailed(message: ""),
            "startup_failed, verify_fix_exhausted, or stage_failed",
          )
      }
    },
  )
}

/// Codec for the top-level pipeline workflow's typed error (no schema —
/// errors are not dispatch-boundary payloads).
pub fn pipeline_error_codec() -> codec.Codec(PipelineError) {
  codec.json_codec(pipeline_error_to_json, pipeline_error_decoder())
}

fn pipeline_error_to_json(workflow_error: PipelineError) -> json.Json {
  case workflow_error {
    ProvisionFailed(message: message) ->
      tagged_message("provision_failed", message)
    DevFailed(message: message) -> tagged_message("dev_failed", message)
    VerifyExhausted(rounds: rounds, diagnostics: diagnostics) ->
      json.object([
        #("error", json.string("verify_exhausted")),
        #("rounds", json.int(rounds)),
        #("diagnostics", json.string(diagnostics)),
      ])
    GateRejected(report: report) ->
      json.object([
        #("error", json.string("gate_rejected")),
        #("report", json.string(report)),
      ])
    ReviewRejected(reason: reason) ->
      json.object([
        #("error", json.string("review_rejected")),
        #("reason", json.string(reason)),
      ])
    ReviewTimedOut(deadline_ms: deadline_ms) ->
      json.object([
        #("error", json.string("review_timed_out")),
        #("deadline_ms", json.int(deadline_ms)),
      ])
    ReviewCapExhausted(rounds: rounds) ->
      json.object([
        #("error", json.string("review_cap_exhausted")),
        #("rounds", json.int(rounds)),
      ])
    LandFailed(message: message) -> tagged_message("land_failed", message)
    StageFailed(stage: stage, message: message) ->
      json.object([
        #("error", json.string("stage_failed")),
        #("stage", json.string(stage)),
        #("message", json.string(message)),
      ])
  }
}

fn tagged_message(tag: String, message: String) -> json.Json {
  json.object([
    #("error", json.string(tag)),
    #("message", json.string(message)),
  ])
}

fn pipeline_error_decoder() -> decode.Decoder(PipelineError) {
  use tag <- decode.field("error", decode.string)
  case tag {
    "provision_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(ProvisionFailed(message: message))
    }
    "dev_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(DevFailed(message: message))
    }
    "verify_exhausted" -> {
      use rounds <- decode.field("rounds", decode.int)
      use diagnostics <- decode.field("diagnostics", decode.string)
      decode.success(VerifyExhausted(rounds: rounds, diagnostics: diagnostics))
    }
    "gate_rejected" -> {
      use report <- decode.field("report", decode.string)
      decode.success(GateRejected(report: report))
    }
    "review_rejected" -> {
      use reason <- decode.field("reason", decode.string)
      decode.success(ReviewRejected(reason: reason))
    }
    "review_timed_out" -> {
      use deadline_ms <- decode.field("deadline_ms", decode.int)
      decode.success(ReviewTimedOut(deadline_ms: deadline_ms))
    }
    "review_cap_exhausted" -> {
      use rounds <- decode.field("rounds", decode.int)
      decode.success(ReviewCapExhausted(rounds: rounds))
    }
    "land_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(LandFailed(message: message))
    }
    "stage_failed" -> {
      use stage <- decode.field("stage", decode.string)
      use message <- decode.field("message", decode.string)
      decode.success(StageFailed(stage: stage, message: message))
    }
    _ -> decode.failure(StageFailed(stage: "", message: ""), "pipeline error tag")
  }
}

/// Codec for the pipeline status query reply (no schema — query replies are
/// not dispatch-boundary payloads).
pub fn pipeline_status_codec() -> codec.Codec(PipelineStatus) {
  codec.json_codec(
    fn(status: PipelineStatus) {
      json.object([
        #("phase", json.string(status.phase)),
        #("round", json.int(status.round)),
      ])
    },
    {
      use phase <- decode.field("phase", decode.string)
      use round <- decode.field("round", decode.int)
      decode.success(PipelineStatus(phase: phase, round: round))
    },
  )
}

/// Codec for the dev child status query reply (no schema — query replies
/// are not dispatch-boundary payloads).
pub fn dev_flow_status_codec() -> codec.Codec(DevFlowStatus) {
  codec.json_codec(
    fn(status: DevFlowStatus) {
      json.object([
        #("phase", json.string(status.phase)),
        #("round", json.int(status.round)),
      ])
    },
    {
      use phase <- decode.field("phase", decode.string)
      use round <- decode.field("round", decode.int)
      decode.success(DevFlowStatus(phase: phase, round: round))
    },
  )
}

/// Zero value used only inside decoder failure branches, where
/// `gleam/dynamic/decode` requires a representative value of the decoded
/// type. It never escapes a successful decode.
fn fallback_gate_input() -> GateInput {
  GateInput(
    workspace: Workspace(
      path: "",
      branch: "",
      placement: Local,
      isolation: Worktree,
    ),
    files_touched: [],
    scope: WorkspaceWide,
  )
}
