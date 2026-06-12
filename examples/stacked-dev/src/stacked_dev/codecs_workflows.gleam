//// JSON codecs for the workflow-level inputs, outputs, typed errors, and
//// status replies of the three workflow entries.
////
//// Activity-payload codecs live in `stacked_dev/codecs_core` (workspace,
//// startup, dev, checks) and `stacked_dev/codecs_flow` (gate, review,
//// land).

import aion/codec
import gleam/dynamic/decode
import gleam/json
import stacked_dev/codecs_core
import stacked_dev/types.{
  type OnatoppError, type OnatoppInput, type OnatoppResult, type OnatoppStatus,
  type StackedDevError, type StackedDevInput, type StackedDevResult,
  type StackedDevStatus, DevFailed, GateRejected, LandFailed, OnatoppInput,
  OnatoppResult, OnatoppStageFailed, OnatoppStatus, ProvisionFailed,
  ReviewCapExhausted, ReviewRejected, ReviewTimedOut, StackedDevInput,
  StackedDevResult, StackedDevStatus, StageFailed, StartupFailed,
  VerifyExhausted, VerifyFixExhausted,
}

/// Codec for the `onatopp_dev` workflow input.
pub fn onatopp_input_codec() -> codec.Codec(OnatoppInput) {
  codec.json_codec(
    fn(input: OnatoppInput) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("brief", json.string(input.brief)),
        #("design", json.string(input.design)),
        #("checklist", json.string(input.checklist)),
        #("stories", json.array(input.stories, json.string)),
        #("verify_fix_cap", json.int(input.verify_fix_cap)),
        #("round_backoff_ms", json.int(input.round_backoff_ms)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use brief <- decode.field("brief", decode.string)
      use design <- decode.field("design", decode.string)
      use checklist <- decode.field("checklist", decode.string)
      use stories <- decode.field("stories", decode.list(decode.string))
      use verify_fix_cap <- decode.field("verify_fix_cap", decode.int)
      use round_backoff_ms <- decode.field("round_backoff_ms", decode.int)
      decode.success(OnatoppInput(
        workspace: workspace,
        brief: brief,
        design: design,
        checklist: checklist,
        stories: stories,
        verify_fix_cap: verify_fix_cap,
        round_backoff_ms: round_backoff_ms,
      ))
    },
  )
}

/// Codec for the `onatopp_dev` workflow output.
pub fn onatopp_result_codec() -> codec.Codec(OnatoppResult) {
  codec.json_codec(
    fn(result: OnatoppResult) {
      json.object([
        #("dev_result", codecs_core.dev_result_to_json(result.dev_result)),
        #("build_warm", codecs_core.build_warm_to_json(result.build_warm)),
        #("verify_rounds", json.int(result.verify_rounds)),
      ])
    },
    {
      use dev_result <- decode.field(
        "dev_result",
        codecs_core.dev_result_decoder(),
      )
      use build_warm <- decode.field(
        "build_warm",
        codecs_core.build_warm_decoder(),
      )
      use verify_rounds <- decode.field("verify_rounds", decode.int)
      decode.success(OnatoppResult(
        dev_result: dev_result,
        build_warm: build_warm,
        verify_rounds: verify_rounds,
      ))
    },
  )
}

/// Codec for the `onatopp_dev` workflow's typed error.
pub fn onatopp_error_codec() -> codec.Codec(OnatoppError) {
  codec.json_codec(
    fn(onatopp_error: OnatoppError) {
      case onatopp_error {
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
        OnatoppStageFailed(stage: stage, message: message) ->
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
          decode.success(OnatoppStageFailed(stage: stage, message: message))
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

/// Codec for the `stacked_dev` workflow input. All four loop/deadline
/// parameters are required fields — open question Q5.
pub fn stacked_dev_input_codec() -> codec.Codec(StackedDevInput) {
  codec.json_codec(
    fn(input: StackedDevInput) {
      json.object([
        #("repo_root", json.string(input.repo_root)),
        #("brief_id", json.string(input.brief_id)),
        #("reviewers", json.array(input.reviewers, json.string)),
        #("base_ref", json.string(input.base_ref)),
        #(
          "placement",
          json.string(codecs_core.placement_to_string(input.placement)),
        ),
        #(
          "isolation",
          json.string(codecs_core.isolation_to_string(input.isolation)),
        ),
        #("brief", json.string(input.brief)),
        #("design", json.string(input.design)),
        #("checklist", json.string(input.checklist)),
        #("stories", json.array(input.stories, json.string)),
        #("verify_fix_cap", json.int(input.verify_fix_cap)),
        #("review_cap", json.int(input.review_cap)),
        #("round_backoff_ms", json.int(input.round_backoff_ms)),
        #("review_deadline_ms", json.int(input.review_deadline_ms)),
      ])
    },
    {
      use provision <- decode.then(codecs_core.provision_input_decoder())
      use reviewers <- decode.field("reviewers", decode.list(decode.string))
      use brief <- decode.field("brief", decode.string)
      use design <- decode.field("design", decode.string)
      use checklist <- decode.field("checklist", decode.string)
      use stories <- decode.field("stories", decode.list(decode.string))
      use verify_fix_cap <- decode.field("verify_fix_cap", decode.int)
      use review_cap <- decode.field("review_cap", decode.int)
      use round_backoff_ms <- decode.field("round_backoff_ms", decode.int)
      use review_deadline_ms <- decode.field("review_deadline_ms", decode.int)
      decode.success(StackedDevInput(
        repo_root: provision.repo_root,
        brief_id: provision.brief_id,
        reviewers: reviewers,
        base_ref: provision.base_ref,
        placement: provision.placement,
        isolation: provision.isolation,
        brief: brief,
        design: design,
        checklist: checklist,
        stories: stories,
        verify_fix_cap: verify_fix_cap,
        review_cap: review_cap,
        round_backoff_ms: round_backoff_ms,
        review_deadline_ms: review_deadline_ms,
      ))
    },
  )
}

/// Codec for the `stacked_dev` workflow output.
pub fn stacked_dev_result_codec() -> codec.Codec(StackedDevResult) {
  codec.json_codec(
    fn(result: StackedDevResult) {
      json.object([
        #("branch", json.string(result.branch)),
        #("merged_into", json.string(result.merged_into)),
        #("session_id", json.string(result.session_id)),
        #("build_warm", codecs_core.build_warm_to_json(result.build_warm)),
        #("verify_rounds", json.int(result.verify_rounds)),
        #("review_rounds", json.int(result.review_rounds)),
      ])
    },
    {
      use branch <- decode.field("branch", decode.string)
      use merged_into <- decode.field("merged_into", decode.string)
      use session_id <- decode.field("session_id", decode.string)
      use build_warm <- decode.field(
        "build_warm",
        codecs_core.build_warm_decoder(),
      )
      use verify_rounds <- decode.field("verify_rounds", decode.int)
      use review_rounds <- decode.field("review_rounds", decode.int)
      decode.success(StackedDevResult(
        branch: branch,
        merged_into: merged_into,
        session_id: session_id,
        build_warm: build_warm,
        verify_rounds: verify_rounds,
        review_rounds: review_rounds,
      ))
    },
  )
}

/// Codec for the `stacked_dev` workflow's typed error.
pub fn stacked_dev_error_codec() -> codec.Codec(StackedDevError) {
  codec.json_codec(stacked_dev_error_to_json, stacked_dev_error_decoder())
}

fn stacked_dev_error_to_json(workflow_error: StackedDevError) -> json.Json {
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

fn stacked_dev_error_decoder() -> decode.Decoder(StackedDevError) {
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
    _ ->
      decode.failure(
        StageFailed(stage: "", message: ""),
        "stacked-dev error tag",
      )
  }
}

/// Codec for the `stacked_dev_status` query reply.
pub fn stacked_dev_status_codec() -> codec.Codec(StackedDevStatus) {
  codec.json_codec(
    fn(status: StackedDevStatus) {
      json.object([
        #("phase", json.string(status.phase)),
        #("round", json.int(status.round)),
      ])
    },
    {
      use phase <- decode.field("phase", decode.string)
      use round <- decode.field("round", decode.int)
      decode.success(StackedDevStatus(phase: phase, round: round))
    },
  )
}

/// Codec for the `onatopp_dev_status` query reply.
pub fn onatopp_status_codec() -> codec.Codec(OnatoppStatus) {
  codec.json_codec(
    fn(status: OnatoppStatus) {
      json.object([
        #("phase", json.string(status.phase)),
        #("round", json.int(status.round)),
      ])
    },
    {
      use phase <- decode.field("phase", decode.string)
      use round <- decode.field("round", decode.int)
      decode.success(OnatoppStatus(phase: phase, round: round))
    },
  )
}
