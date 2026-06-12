//// JSON codecs for the review and land activity payloads.
////
//// Workspace/startup/dev/check codecs live in `{{name}}/codecs_core`;
//// workflow-level input/output/error/status codecs (including the gate
//// child's, generated from `schemas/`) live in
//// `{{name}}/codecs_workflows`. The `request_review` codec embeds the
//// gate result, so it reuses that module's schema-generated encoder and
//// decoder rather than restating the shape.

import {{name}}/codecs_core
import {{name}}/codecs_workflows
import {{name}}/types.{
  type LandInput, type Landed, type ReviewAck, type ReviewNote,
  type ReviewRequest, type ReviewVerdict, Approve, LandInput, Landed, Reject,
  RequestChanges, ReviewAck, ReviewNote, ReviewRequest, ReviewVerdict,
}
import aion/codec
import gleam/dynamic/decode
import gleam/json

/// Codec for the `request_review` activity input.
pub fn review_request_codec() -> codec.Codec(ReviewRequest) {
  codec.json_codec(
    fn(request: ReviewRequest) {
      json.object([
        #("workspace", codecs_core.workspace_to_json(request.workspace)),
        #("brief_id", json.string(request.brief_id)),
        #("reviewers", json.array(request.reviewers, json.string)),
        #("dev_result", codecs_core.dev_result_to_json(request.dev_result)),
        #(
          "gate_result",
          codecs_workflows.gate_result_to_json(request.gate_result),
        ),
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
      use gate_result <- decode.field(
        "gate_result",
        codecs_workflows.gate_result_decoder(),
      )
      decode.success(ReviewRequest(
        workspace: workspace,
        brief_id: brief_id,
        reviewers: reviewers,
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
/// consumes (notes flow to the agent as data, one JSON array, not prose).
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
        #("repo_root", json.string(input.repo_root)),
        #("base_ref", json.string(input.base_ref)),
        #("dev_result", codecs_core.dev_result_to_json(input.dev_result)),
      ])
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
      decode.success(LandInput(
        workspace: workspace,
        repo_root: repo_root,
        base_ref: base_ref,
        dev_result: dev_result,
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
