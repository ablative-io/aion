//// JSON codecs for the workflow-level inputs, outputs, typed errors, and
//// status replies of the three workflow entries.
////
//// Activity-payload codecs live in `stacked_dev/codecs_core` (workspace,
//// startup, dev, checks) and `stacked_dev/codecs_flow` (gate, review,
//// land).

import aion/codec
import aion_stacked_dev_io as stage_io
import gleam/dynamic/decode
import gleam/json
import stacked_dev/codecs_brief
import stacked_dev/codecs_core
import stacked_dev/types.{
  type BriefDevError, type BriefDevInput, type BriefDevResult,
  type BriefDevStatus, type DriftedRequirement, type StackedDevError,
  type StackedDevInput, type StackedDevResult, type StackedDevStatus,
  BriefDevInput, BriefDevResult, BriefDevStageFailed, BriefDevStatus, DevBlocked,
  DevBlockedInChild, DevFailed, DriftedRequirement, GateRejected,
  HardenRegressed, HardenRegressedInChild, LandFailed, ProvisionFailed,
  ReviewCapExhausted, ReviewDrifted, ReviewDriftedInChild, ReviewRejected,
  ReviewTimedOut, ScoutFailed, ScoutFailedInChild, StackedDevInput,
  StackedDevResult, StackedDevStatus, StageFailed, VerifyExhausted,
  VerifyFixExhausted,
}

/// Codec for the `brief_dev` workflow input: the v2 brief document and the
/// pre-resolved reference context, plus the two required loop parameters
/// (BD-003 R2).
pub fn brief_dev_input_codec() -> codec.Codec(BriefDevInput) {
  codec.json_codec(
    fn(input: BriefDevInput) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("document", codecs_brief.brief_document_to_json(input.document)),
        #("context", codecs_brief.resolved_context_to_json(input.context)),
        #("verify_fix_cap", json.int(input.verify_fix_cap)),
        #("round_backoff_ms", json.int(input.round_backoff_ms)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use document <- decode.field(
        "document",
        codecs_brief.brief_document_decoder(),
      )
      use context <- decode.field(
        "context",
        codecs_brief.resolved_context_decoder(),
      )
      use verify_fix_cap <- decode.field("verify_fix_cap", decode.int)
      use round_backoff_ms <- decode.field("round_backoff_ms", decode.int)
      decode.success(BriefDevInput(
        workspace: workspace,
        document: document,
        context: context,
        verify_fix_cap: verify_fix_cap,
        round_backoff_ms: round_backoff_ms,
      ))
    },
  )
}

/// Codec for the `brief_dev` workflow output: the three generated stage
/// reports, the verify-round count, and the advisory warm-build outcome
/// (BD-003 R2).
pub fn brief_dev_result_codec() -> codec.Codec(BriefDevResult) {
  codec.json_codec(
    fn(result: BriefDevResult) {
      json.object([
        #("scout", stage_io.scout_report_to_json(result.scout)),
        #("dev", stage_io.dev_report_to_json(result.dev)),
        #("review", stage_io.review_report_to_json(result.review)),
        #("verify_rounds", json.int(result.verify_rounds)),
        #("build_warm", codecs_core.build_warm_to_json(result.build_warm)),
      ])
    },
    {
      use scout <- decode.field("scout", stage_io.scout_report_decoder())
      use dev <- decode.field("dev", stage_io.dev_report_decoder())
      use review <- decode.field("review", stage_io.review_report_decoder())
      use verify_rounds <- decode.field("verify_rounds", decode.int)
      use build_warm <- decode.field(
        "build_warm",
        codecs_core.build_warm_decoder(),
      )
      decode.success(BriefDevResult(
        scout: scout,
        dev: dev,
        review: review,
        verify_rounds: verify_rounds,
        build_warm: build_warm,
      ))
    },
  )
}

/// Codec for the `brief_dev` workflow's typed error. Wire tags: scout_failed,
/// dev_blocked, verify_fix_exhausted, review_drifted, harden_regressed,
/// stage_failed. An unknown tag fails the decode naming the expected tags and
/// is never mapped onto a variant (BD-003 R2).
pub fn brief_dev_error_codec() -> codec.Codec(BriefDevError) {
  codec.json_codec(brief_dev_error_to_json, brief_dev_error_decoder())
}

fn brief_dev_error_to_json(brief_dev_error: BriefDevError) -> json.Json {
  case brief_dev_error {
    ScoutFailed(message: message) ->
      json.object([
        #("error", json.string("scout_failed")),
        #("message", json.string(message)),
      ])
    DevBlocked(requirement_ids: requirement_ids) ->
      json.object([
        #("error", json.string("dev_blocked")),
        #("requirement_ids", json.array(requirement_ids, json.string)),
      ])
    VerifyFixExhausted(rounds: rounds, diagnostics: diagnostics) ->
      json.object([
        #("error", json.string("verify_fix_exhausted")),
        #("rounds", json.int(rounds)),
        #("diagnostics", json.string(diagnostics)),
      ])
    ReviewDrifted(drifted: drifted) ->
      json.object([
        #("error", json.string("review_drifted")),
        #("drifted", json.array(drifted, drifted_requirement_to_json)),
      ])
    HardenRegressed(diagnostics: diagnostics) ->
      json.object([
        #("error", json.string("harden_regressed")),
        #("diagnostics", json.string(diagnostics)),
      ])
    BriefDevStageFailed(stage: stage, message: message) ->
      json.object([
        #("error", json.string("stage_failed")),
        #("stage", json.string(stage)),
        #("message", json.string(message)),
      ])
  }
}

fn brief_dev_error_decoder() -> decode.Decoder(BriefDevError) {
  use tag <- decode.field("error", decode.string)
  case tag {
    "scout_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(ScoutFailed(message: message))
    }
    "dev_blocked" -> {
      use requirement_ids <- decode.field(
        "requirement_ids",
        decode.list(decode.string),
      )
      decode.success(DevBlocked(requirement_ids: requirement_ids))
    }
    "verify_fix_exhausted" -> {
      use rounds <- decode.field("rounds", decode.int)
      use diagnostics <- decode.field("diagnostics", decode.string)
      decode.success(VerifyFixExhausted(
        rounds: rounds,
        diagnostics: diagnostics,
      ))
    }
    "review_drifted" -> {
      use drifted <- decode.field(
        "drifted",
        decode.list(drifted_requirement_decoder()),
      )
      decode.success(ReviewDrifted(drifted: drifted))
    }
    "harden_regressed" -> {
      use diagnostics <- decode.field("diagnostics", decode.string)
      decode.success(HardenRegressed(diagnostics: diagnostics))
    }
    "stage_failed" -> {
      use stage <- decode.field("stage", decode.string)
      use message <- decode.field("message", decode.string)
      decode.success(BriefDevStageFailed(stage: stage, message: message))
    }
    _ ->
      decode.failure(
        ScoutFailed(message: ""),
        "scout_failed, dev_blocked, verify_fix_exhausted, review_drifted,"
          <> " harden_regressed, or stage_failed",
      )
  }
}

fn drifted_requirement_to_json(drifted: DriftedRequirement) -> json.Json {
  json.object([
    #("id", json.string(drifted.id)),
    #("issues", json.array(drifted.issues, json.string)),
  ])
}

fn drifted_requirement_decoder() -> decode.Decoder(DriftedRequirement) {
  use id <- decode.field("id", decode.string)
  use issues <- decode.field("issues", decode.list(decode.string))
  decode.success(DriftedRequirement(id: id, issues: issues))
}

/// Codec for the `stacked_dev` workflow input. Carries the v2 brief document
/// and the pre-resolved reference context in place of the four document
/// strings (BD-005 R3, ADR-008); the old shape no longer decodes (ADR-002).
/// All four loop/deadline parameters stay required fields (CN2).
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
        #(
          "brief_document",
          codecs_brief.brief_document_to_json(input.brief_document),
        ),
        #(
          "resolved_context",
          codecs_brief.resolved_context_to_json(input.resolved_context),
        ),
        #("verify_fix_cap", json.int(input.verify_fix_cap)),
        #("review_cap", json.int(input.review_cap)),
        #("round_backoff_ms", json.int(input.round_backoff_ms)),
        #("review_deadline_ms", json.int(input.review_deadline_ms)),
      ])
    },
    {
      use provision <- decode.then(codecs_core.provision_input_decoder())
      use reviewers <- decode.field("reviewers", decode.list(decode.string))
      use brief_document <- decode.field(
        "brief_document",
        codecs_brief.brief_document_decoder(),
      )
      use resolved_context <- decode.field(
        "resolved_context",
        codecs_brief.resolved_context_decoder(),
      )
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
        brief_document: brief_document,
        resolved_context: resolved_context,
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

/// JSON encoder for a `StackedDevError`, exposed so the dispatch codecs can
/// embed exactly this encoding under a failed outcome's `"error"` key — one
/// error encoding shared between the two workflows, never a second copy (P4).
pub fn stacked_dev_error_to_json(workflow_error: StackedDevError) -> json.Json {
  case workflow_error {
    ProvisionFailed(message: message) ->
      tagged_message("provision_failed", message)
    ScoutFailedInChild(message: message) ->
      tagged_message("scout_failed", message)
    DevBlockedInChild(requirement_ids: requirement_ids) ->
      json.object([
        #("error", json.string("dev_blocked")),
        #("requirement_ids", json.array(requirement_ids, json.string)),
      ])
    DevFailed(message: message) -> tagged_message("dev_failed", message)
    VerifyExhausted(rounds: rounds, diagnostics: diagnostics) ->
      json.object([
        #("error", json.string("verify_exhausted")),
        #("rounds", json.int(rounds)),
        #("diagnostics", json.string(diagnostics)),
      ])
    ReviewDriftedInChild(drifted: drifted) ->
      json.object([
        #("error", json.string("review_drifted")),
        #("drifted", json.array(drifted, drifted_requirement_to_json)),
      ])
    HardenRegressedInChild(diagnostics: diagnostics) ->
      json.object([
        #("error", json.string("harden_regressed")),
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

/// JSON decoder for a `StackedDevError`, exposed for the dispatch codecs (see
/// `stacked_dev_error_to_json`).
pub fn stacked_dev_error_decoder() -> decode.Decoder(StackedDevError) {
  use tag <- decode.field("error", decode.string)
  case tag {
    "provision_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(ProvisionFailed(message: message))
    }
    "scout_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(ScoutFailedInChild(message: message))
    }
    "dev_blocked" -> {
      use requirement_ids <- decode.field(
        "requirement_ids",
        decode.list(decode.string),
      )
      decode.success(DevBlockedInChild(requirement_ids: requirement_ids))
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
    "review_drifted" -> {
      use drifted <- decode.field(
        "drifted",
        decode.list(drifted_requirement_decoder()),
      )
      decode.success(ReviewDriftedInChild(drifted: drifted))
    }
    "harden_regressed" -> {
      use diagnostics <- decode.field("diagnostics", decode.string)
      decode.success(HardenRegressedInChild(diagnostics: diagnostics))
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

/// Codec for the `brief_dev_status` query reply.
pub fn brief_dev_status_codec() -> codec.Codec(BriefDevStatus) {
  codec.json_codec(
    fn(status: BriefDevStatus) {
      json.object([
        #("phase", json.string(status.phase)),
        #("round", json.int(status.round)),
      ])
    },
    {
      use phase <- decode.field("phase", decode.string)
      use round <- decode.field("round", decode.int)
      decode.success(BriefDevStatus(phase: phase, round: round))
    },
  )
}
