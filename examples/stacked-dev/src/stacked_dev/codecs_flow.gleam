//// JSON codecs for the gate, review, land, workflow-level, and status types.
////
//// Workspace/startup/dev/check codecs live in `stacked_dev/codecs_core`.

import aion/codec
import gleam/dynamic/decode
import gleam/json
import stacked_dev/codecs_core
import stacked_dev/types.{
  type GateError, type GateInput, type GateResult, type GateScope,
  type GateVerdict, type LandInput, type Landed, type OnatoppError,
  type OnatoppInput, type OnatoppResult, type OnatoppStatus, type ReviewAck,
  type ReviewNote, type ReviewRequest, type ReviewVerdict, type StackedDevError,
  type StackedDevInput, type StackedDevResult, type StackedDevStatus,
  AffectedClosure, Approve, DevFailed, GateFail, GateInput, GatePass,
  GateRejected, GateResult, GateStageFailed, LandFailed, LandInput, Landed,
  OnatoppInput, OnatoppResult, OnatoppStageFailed, OnatoppStatus,
  ProvisionFailed, Reject, RequestChanges, ReviewAck, ReviewCapExhausted,
  ReviewNote, ReviewRejected, ReviewRequest, ReviewTimedOut, ReviewVerdict,
  StackedDevInput, StackedDevResult, StackedDevStatus, StageFailed,
  StartupFailed, VerifyExhausted, VerifyFixExhausted, WorkspaceWide,
}

/// Codec for the `gate` child input.
pub fn gate_input_codec() -> codec.Codec(GateInput) {
  codec.json_codec(
    fn(input: GateInput) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("files_touched", json.array(input.files_touched, json.string)),
        #("scope", gate_scope_to_json(input.scope)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use files_touched <- decode.field(
        "files_touched",
        decode.list(decode.string),
      )
      use scope <- decode.field("scope", gate_scope_decoder())
      decode.success(GateInput(
        workspace: workspace,
        files_touched: files_touched,
        scope: scope,
      ))
    },
  )
}

fn gate_scope_to_json(scope: GateScope) -> json.Json {
  case scope {
    WorkspaceWide -> json.object([#("kind", json.string("workspace_wide"))])
    AffectedClosure(modules: modules) ->
      json.object([
        #("kind", json.string("affected_closure")),
        #("modules", json.array(modules, json.string)),
      ])
  }
}

fn gate_scope_decoder() -> decode.Decoder(GateScope) {
  use kind <- decode.field("kind", decode.string)
  case kind {
    "workspace_wide" -> decode.success(WorkspaceWide)
    "affected_closure" -> {
      use modules <- decode.field("modules", decode.list(decode.string))
      decode.success(AffectedClosure(modules: modules))
    }
    _ -> decode.failure(WorkspaceWide, "workspace_wide or affected_closure")
  }
}

/// Codec for the `gate` child output (also the `full_checks` activity
/// output).
pub fn gate_result_codec() -> codec.Codec(GateResult) {
  codec.json_codec(gate_result_to_json, gate_result_decoder())
}

fn gate_result_to_json(result: GateResult) -> json.Json {
  json.object([#("verdict", gate_verdict_to_json(result.verdict))])
}

fn gate_result_decoder() -> decode.Decoder(GateResult) {
  use verdict <- decode.field("verdict", gate_verdict_decoder())
  decode.success(GateResult(verdict: verdict))
}

fn gate_verdict_to_json(verdict: GateVerdict) -> json.Json {
  case verdict {
    GatePass -> json.object([#("outcome", json.string("pass"))])
    GateFail(report: report) ->
      json.object([
        #("outcome", json.string("fail")),
        #("report", json.string(report)),
      ])
  }
}

fn gate_verdict_decoder() -> decode.Decoder(GateVerdict) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "pass" -> decode.success(GatePass)
    "fail" -> {
      use report <- decode.field("report", decode.string)
      decode.success(GateFail(report: report))
    }
    _ -> decode.failure(GatePass, "pass or fail")
  }
}

/// Codec for the `gate` child's typed error.
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

/// Codec for the `request_review` activity input.
pub fn review_request_codec() -> codec.Codec(ReviewRequest) {
  codec.json_codec(
    fn(request: ReviewRequest) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(request.workspace)),
        #("brief_id", json.string(request.brief_id)),
        #("dev_result", codecs_core.dev_result_to_json(request.dev_result)),
        #("gate_result", gate_result_to_json(request.gate_result)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use brief_id <- decode.field("brief_id", decode.string)
      use dev_result <- decode.field(
        "dev_result",
        codecs_core.dev_result_decoder(),
      )
      use gate_result <- decode.field("gate_result", gate_result_decoder())
      decode.success(ReviewRequest(
        workspace: workspace,
        brief_id: brief_id,
        dev_result: dev_result,
        gate_result: gate_result,
      ))
    },
  )
}

/// Codec for the `request_review` activity output.
pub fn review_ack_codec() -> codec.Codec(ReviewAck) {
  codec.json_codec(
    fn(ack: ReviewAck) {
      json.object([#("request_id", json.string(ack.request_id))])
    },
    {
      use request_id <- decode.field("request_id", decode.string)
      decode.success(ReviewAck(request_id: request_id))
    },
  )
}

/// JSON encoder for one structured review note.
pub fn review_note_to_json(note: ReviewNote) -> json.Json {
  json.object([
    #("file", json.string(note.file)),
    #("line", json.int(note.line)),
    #("note", json.string(note.note)),
  ])
}

fn review_note_decoder() -> decode.Decoder(ReviewNote) {
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use note <- decode.field("note", decode.string)
  decode.success(ReviewNote(file: file, line: line, note: note))
}

/// Encode structured review notes as the feedback string `dev_resume`
/// consumes (open question Q3: notes flow to the agent as data, one JSON
/// array, not prose).
pub fn review_notes_feedback(notes: List(ReviewNote)) -> String {
  json.array(notes, review_note_to_json)
  |> json.to_string
}

/// Codec for the `review_verdict` signal payload.
///
/// Wire shapes:
/// `{"decision":"approve"}`,
/// `{"decision":"request_changes","notes":[{"file":..,"line":..,"note":..}]}`,
/// `{"decision":"reject","reason":".."}`.
pub fn review_verdict_codec() -> codec.Codec(ReviewVerdict) {
  codec.json_codec(
    fn(verdict: ReviewVerdict) {
      case verdict.decision {
        Approve -> json.object([#("decision", json.string("approve"))])
        RequestChanges(notes: notes) ->
          json.object([
            #("decision", json.string("request_changes")),
            #("notes", json.array(notes, review_note_to_json)),
          ])
        Reject(reason: reason) ->
          json.object([
            #("decision", json.string("reject")),
            #("reason", json.string(reason)),
          ])
      }
    },
    {
      use decision <- decode.field("decision", decode.string)
      case decision {
        "approve" -> decode.success(ReviewVerdict(decision: Approve))
        "request_changes" -> {
          use notes <- decode.field("notes", decode.list(review_note_decoder()))
          decode.success(ReviewVerdict(decision: RequestChanges(notes: notes)))
        }
        "reject" -> {
          use reason <- decode.field("reason", decode.string)
          decode.success(ReviewVerdict(decision: Reject(reason: reason)))
        }
        _ ->
          decode.failure(
            ReviewVerdict(decision: Approve),
            "approve, request_changes, or reject",
          )
      }
    },
  )
}

/// Codec for the `land` activity input.
pub fn land_input_codec() -> codec.Codec(LandInput) {
  codec.json_codec(
    fn(input: LandInput) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("dev_result", codecs_core.dev_result_to_json(input.dev_result)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use dev_result <- decode.field(
        "dev_result",
        codecs_core.dev_result_decoder(),
      )
      decode.success(LandInput(workspace: workspace, dev_result: dev_result))
    },
  )
}

/// Codec for the `land` activity output.
pub fn landed_codec() -> codec.Codec(Landed) {
  codec.json_codec(
    fn(landed: Landed) {
      json.object([
        #("pr_url", json.string(landed.pr_url)),
        #("merge_commit", json.string(landed.merge_commit)),
      ])
    },
    {
      use pr_url <- decode.field("pr_url", decode.string)
      use merge_commit <- decode.field("merge_commit", decode.string)
      decode.success(Landed(pr_url: pr_url, merge_commit: merge_commit))
    },
  )
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
        #("brief_id", json.string(input.brief_id)),
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
      use brief <- decode.field("brief", decode.string)
      use design <- decode.field("design", decode.string)
      use checklist <- decode.field("checklist", decode.string)
      use stories <- decode.field("stories", decode.list(decode.string))
      use verify_fix_cap <- decode.field("verify_fix_cap", decode.int)
      use review_cap <- decode.field("review_cap", decode.int)
      use round_backoff_ms <- decode.field("round_backoff_ms", decode.int)
      use review_deadline_ms <- decode.field("review_deadline_ms", decode.int)
      decode.success(StackedDevInput(
        brief_id: provision.brief_id,
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
        #("pr_url", json.string(result.pr_url)),
        #("merge_commit", json.string(result.merge_commit)),
        #("session_id", json.string(result.session_id)),
        #("build_warm", codecs_core.build_warm_to_json(result.build_warm)),
        #("verify_rounds", json.int(result.verify_rounds)),
        #("review_rounds", json.int(result.review_rounds)),
      ])
    },
    {
      use pr_url <- decode.field("pr_url", decode.string)
      use merge_commit <- decode.field("merge_commit", decode.string)
      use session_id <- decode.field("session_id", decode.string)
      use build_warm <- decode.field(
        "build_warm",
        codecs_core.build_warm_decoder(),
      )
      use verify_rounds <- decode.field("verify_rounds", decode.int)
      use review_rounds <- decode.field("review_rounds", decode.int)
      decode.success(StackedDevResult(
        pr_url: pr_url,
        merge_commit: merge_commit,
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
