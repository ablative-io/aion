//// JSON codecs for the gate, review, and land activity/child payloads.
////
//// Workspace/startup/dev/check codecs live in `stacked_dev/codecs_core`;
//// workflow-level input/output/error/status codecs live in
//// `stacked_dev/codecs_workflows`.

import aion/codec
import aion_stacked_dev_io as stage_io
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option
import stacked_dev/codecs_brief
import stacked_dev/codecs_brief_blocks as blocks
import stacked_dev/codecs_core
import stacked_dev/types.{
  type EnrichInput, type Enrichment, type GateError, type GateInput,
  type GateResult, type GateScope, type GateVerdict, type LandInput, type Landed,
  type ReviewAck, type ReviewInput, type ReviewNote, type ReviewRequest,
  type ReviewVerdict, type TeardownInput, type TornDown, AffectedClosure,
  Approve, DevEnrichment, EnrichInput, ExecutionEnrichment, GateFail, GateInput,
  GatePass, GateResult, GateStageFailed, LandInput, Landed, Reject,
  RequestChanges, ReviewAck, ReviewEnrichment, ReviewInput, ReviewNote,
  ReviewRequest, ReviewVerdict, ScoutEnrichment, TeardownInput, TornDown,
  WorkspaceWide,
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
        #("reviewers", json.array(request.reviewers, json.string)),
        #("dev_result", codecs_core.dev_result_to_json(request.dev_result)),
        #("gate_result", gate_result_to_json(request.gate_result)),
        #("workflow_id", json.string(request.workflow_id)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use brief_id <- decode.field("brief_id", decode.string)
      use reviewers <- decode.field("reviewers", decode.list(decode.string))
      use dev_result <- decode.field(
        "dev_result",
        codecs_core.dev_result_decoder(),
      )
      use gate_result <- decode.field("gate_result", gate_result_decoder())
      use workflow_id <- decode.field("workflow_id", decode.string)
      decode.success(ReviewRequest(
        workspace: workspace,
        brief_id: brief_id,
        reviewers: reviewers,
        dev_result: dev_result,
        gate_result: gate_result,
        workflow_id: workflow_id,
      ))
    },
  )
}

/// Codec for the `dev_review` activity input: the workspace and the projected
/// review prompt (BD-003). Distinct from `review_request_codec`, which is the
/// outer arc's human review-request payload.
pub fn review_input_codec() -> codec.Codec(ReviewInput) {
  codec.json_codec(
    fn(input: ReviewInput) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("prompt", json.string(input.prompt)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use prompt <- decode.field("prompt", decode.string)
      decode.success(ReviewInput(workspace: workspace, prompt: prompt))
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

/// Codec for the `enrich_brief` activity input. Hand-written: the
/// `Enrichment` union is outside the codegen v1 subset (codegen rejects
/// unions). The activity OUTPUT codec is BD-001's brief document codec used
/// directly — no second brief document codec exists.
///
/// Wire shape: `{"workspace": .., "document": .., "enrichment": {"stage":
/// "scout"|"dev"|"review"|"execution", "report"|"block": ..}}` — the stage
/// payload rides under `"report"` for the three stage variants and under
/// `"block"` for the execution variant.
pub fn enrich_input_codec() -> codec.Codec(EnrichInput) {
  codec.json_codec(
    fn(input: EnrichInput) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("document", codecs_brief.brief_document_to_json(input.document)),
        #("enrichment", enrichment_to_json(input.enrichment)),
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
      use enrichment <- decode.field("enrichment", enrichment_decoder())
      decode.success(EnrichInput(
        workspace: workspace,
        document: document,
        enrichment: enrichment,
      ))
    },
  )
}

fn enrichment_to_json(enrichment: Enrichment) -> json.Json {
  case enrichment {
    ScoutEnrichment(report: report) ->
      json.object([
        #("stage", json.string("scout")),
        #("report", stage_io.scout_report_to_json(report)),
      ])
    DevEnrichment(report: report) ->
      json.object([
        #("stage", json.string("dev")),
        #("report", stage_io.dev_report_to_json(report)),
      ])
    ReviewEnrichment(report: report) ->
      json.object([
        #("stage", json.string("review")),
        #("report", stage_io.review_report_to_json(report)),
      ])
    ExecutionEnrichment(block: block) ->
      json.object([
        #("stage", json.string("execution")),
        #("block", blocks.execution_block_to_json(block)),
      ])
  }
}

fn enrichment_decoder() -> decode.Decoder(Enrichment) {
  use stage <- decode.field("stage", decode.string)
  case stage {
    "scout" -> {
      use report <- decode.field("report", stage_io.scout_report_decoder())
      decode.success(ScoutEnrichment(report: report))
    }
    "dev" -> {
      use report <- decode.field("report", stage_io.dev_report_decoder())
      decode.success(DevEnrichment(report: report))
    }
    "review" -> {
      use report <- decode.field("report", stage_io.review_report_decoder())
      decode.success(ReviewEnrichment(report: report))
    }
    "execution" -> {
      use block <- decode.field("block", blocks.execution_block_decoder())
      decode.success(ExecutionEnrichment(block: block))
    }
    _ ->
      decode.failure(
        ScoutEnrichment(
          report: stage_io.ScoutReport(
            summary: "",
            enrichments: [],
            verification: [],
          ),
        ),
        "scout, dev, review, or execution",
      )
  }
}

/// Codec for the `land` activity input.
pub fn land_input_codec() -> codec.Codec(LandInput) {
  codec.json_codec(
    fn(input: LandInput) {
      let base = [
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("repo_root", json.string(input.repo_root)),
        #("base_ref", json.string(input.base_ref)),
        #("dev_result", codecs_core.dev_result_to_json(input.dev_result)),
      ]
      case input.clone_url {
        option.Some(url) ->
          json.object(list.append(base, [#("clone_url", json.string(url))]))
        option.None -> json.object(base)
      }
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use repo_root <- decode.field("repo_root", decode.string)
      use base_ref <- decode.field("base_ref", decode.string)
      use dev_result <- decode.field(
        "dev_result",
        codecs_core.dev_result_decoder(),
      )
      use clone_url <- decode.optional_field(
        "clone_url",
        option.None,
        decode.map(decode.string, option.Some),
      )
      decode.success(LandInput(
        workspace: workspace,
        repo_root: repo_root,
        base_ref: base_ref,
        dev_result: dev_result,
        clone_url: clone_url,
      ))
    },
  )
}

/// Codec for the `land` activity output.
pub fn landed_codec() -> codec.Codec(Landed) {
  codec.json_codec(
    fn(landed: Landed) {
      json.object([
        #("branch", json.string(landed.branch)),
        #("merged_into", json.string(landed.merged_into)),
      ])
    },
    {
      use branch <- decode.field("branch", decode.string)
      use merged_into <- decode.field("merged_into", decode.string)
      decode.success(Landed(branch: branch, merged_into: merged_into))
    },
  )
}

/// Codec for the `teardown_workspace` activity input.
pub fn teardown_input_codec() -> codec.Codec(TeardownInput) {
  codec.json_codec(
    fn(input: TeardownInput) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(input.workspace)),
        #("repo_root", json.string(input.repo_root)),
      ])
    },
    {
      use workspace <- decode.field(
        "workspace",
        codecs_core.workspace_decoder(),
      )
      use repo_root <- decode.field("repo_root", decode.string)
      decode.success(TeardownInput(
        workspace: workspace,
        repo_root: repo_root,
      ))
    },
  )
}

/// Codec for the `teardown_workspace` activity output.
pub fn torn_down_codec() -> codec.Codec(TornDown) {
  codec.json_codec(
    fn(td: TornDown) {
      json.object([
        #("branch", json.string(td.branch)),
        #("cleaned", json.bool(td.cleaned)),
      ])
    },
    {
      use branch <- decode.field("branch", decode.string)
      use cleaned <- decode.field("cleaned", decode.bool)
      decode.success(TornDown(branch: branch, cleaned: cleaned))
    },
  )
}
