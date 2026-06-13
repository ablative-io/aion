//// Hand-written JSON codecs for the dispatch workflow IO and the
//// `assemble_wave` activity payloads (BD-006).
////
//// These shapes carry unions (the `BriefOutcome` outcome tag, the
//// `DispatchError` tag), so they stay hand-written — codegen v1 rejects
//// unions. The failed-outcome variant embeds the EXISTING stacked_dev error
//// encoding under `"error"` via `codecs_workflows.stacked_dev_error_to_json`,
//// so there is one error encoding shared with the outer arc, never a second
//// copy (P4). A separate module from `codecs_workflows` to keep both under the
//// module-size cap.

import aion/codec
import gleam/dynamic/decode
import gleam/json
import stacked_dev/codecs_brief
import stacked_dev/codecs_core
import stacked_dev/codecs_workflows
import stacked_dev/types.{
  type AssembleInput, type AssembledWave, type BriefOutcome, type DispatchError,
  type DispatchInput, type DispatchResult, type DispatchStatus, type Isolation,
  type Placement, type WaveEntry, AssembleInput, AssembledWave, AssemblyRefused,
  BriefFailed, BriefLanded, BriefSkipped, Copy, DispatchInput, DispatchResult,
  DispatchStageFailed, DispatchStatus, Local, Overlay, Remote, Vm, WaveEntry,
  Worktree,
}

/// Codec for the `dispatch` workflow input: the wave of brief ids plus every
/// shared child parameter and `halt_on_failure`. All twelve fields are
/// required (ADR-001); the encoder emits them in declaration order so the wire
/// shape is stable.
pub fn dispatch_input_codec() -> codec.Codec(DispatchInput) {
  codec.json_codec(
    fn(input: DispatchInput) {
      json.object([
        #("design_dir", json.string(input.design_dir)),
        #("wave", json.array(input.wave, json.string)),
        #("repo_root", json.string(input.repo_root)),
        #("base_ref", json.string(input.base_ref)),
        #("reviewers", json.array(input.reviewers, json.string)),
        #(
          "placement",
          json.string(codecs_core.placement_to_string(input.placement)),
        ),
        #(
          "isolation",
          json.string(codecs_core.isolation_to_string(input.isolation)),
        ),
        #("verify_fix_cap", json.int(input.verify_fix_cap)),
        #("review_cap", json.int(input.review_cap)),
        #("round_backoff_ms", json.int(input.round_backoff_ms)),
        #("review_deadline_ms", json.int(input.review_deadline_ms)),
        #("halt_on_failure", json.bool(input.halt_on_failure)),
      ])
    },
    {
      use design_dir <- decode.field("design_dir", decode.string)
      use wave <- decode.field("wave", decode.list(decode.string))
      use repo_root <- decode.field("repo_root", decode.string)
      use base_ref <- decode.field("base_ref", decode.string)
      use reviewers <- decode.field("reviewers", decode.list(decode.string))
      use placement <- decode.field("placement", placement_decoder())
      use isolation <- decode.field("isolation", isolation_decoder())
      use verify_fix_cap <- decode.field("verify_fix_cap", decode.int)
      use review_cap <- decode.field("review_cap", decode.int)
      use round_backoff_ms <- decode.field("round_backoff_ms", decode.int)
      use review_deadline_ms <- decode.field("review_deadline_ms", decode.int)
      use halt_on_failure <- decode.field("halt_on_failure", decode.bool)
      decode.success(DispatchInput(
        design_dir: design_dir,
        wave: wave,
        repo_root: repo_root,
        base_ref: base_ref,
        reviewers: reviewers,
        placement: placement,
        isolation: isolation,
        verify_fix_cap: verify_fix_cap,
        review_cap: review_cap,
        round_backoff_ms: round_backoff_ms,
        review_deadline_ms: review_deadline_ms,
        halt_on_failure: halt_on_failure,
      ))
    },
  )
}

/// Codec for the `dispatch` workflow output: one tagged outcome per wave
/// entry.
pub fn dispatch_result_codec() -> codec.Codec(DispatchResult) {
  codec.json_codec(
    fn(result: DispatchResult) {
      json.object([
        #("outcomes", json.array(result.outcomes, brief_outcome_to_json)),
      ])
    },
    {
      use outcomes <- decode.field(
        "outcomes",
        decode.list(brief_outcome_decoder()),
      )
      decode.success(DispatchResult(outcomes: outcomes))
    },
  )
}

/// JSON encoder for one `BriefOutcome`: a tagged object whose `"outcome"` is
/// exactly `"landed"`, `"failed"`, or `"skipped"`. The failed variant embeds
/// the stacked_dev error codec's encoding under `"error"`.
pub fn brief_outcome_to_json(outcome: BriefOutcome) -> json.Json {
  case outcome {
    BriefLanded(brief_id: brief_id, branch: branch, merged_into: merged_into) ->
      json.object([
        #("outcome", json.string("landed")),
        #("brief_id", json.string(brief_id)),
        #("branch", json.string(branch)),
        #("merged_into", json.string(merged_into)),
      ])
    BriefFailed(brief_id: brief_id, error: error) ->
      json.object([
        #("outcome", json.string("failed")),
        #("brief_id", json.string(brief_id)),
        #("error", codecs_workflows.stacked_dev_error_to_json(error)),
      ])
    BriefSkipped(brief_id: brief_id, after: after) ->
      json.object([
        #("outcome", json.string("skipped")),
        #("brief_id", json.string(brief_id)),
        #("after", json.string(after)),
      ])
  }
}

/// Decoder for one `BriefOutcome`. An unknown outcome tag fails the decode
/// naming the expected tags and is never mapped onto a variant.
pub fn brief_outcome_decoder() -> decode.Decoder(BriefOutcome) {
  use tag <- decode.field("outcome", decode.string)
  case tag {
    "landed" -> {
      use brief_id <- decode.field("brief_id", decode.string)
      use branch <- decode.field("branch", decode.string)
      use merged_into <- decode.field("merged_into", decode.string)
      decode.success(BriefLanded(
        brief_id: brief_id,
        branch: branch,
        merged_into: merged_into,
      ))
    }
    "failed" -> {
      use brief_id <- decode.field("brief_id", decode.string)
      use error <- decode.field(
        "error",
        codecs_workflows.stacked_dev_error_decoder(),
      )
      decode.success(BriefFailed(brief_id: brief_id, error: error))
    }
    "skipped" -> {
      use brief_id <- decode.field("brief_id", decode.string)
      use after <- decode.field("after", decode.string)
      decode.success(BriefSkipped(brief_id: brief_id, after: after))
    }
    _ ->
      decode.failure(
        BriefLanded(brief_id: "", branch: "", merged_into: ""),
        "landed, failed, or skipped",
      )
  }
}

/// Codec for the `dispatch` workflow's typed error.
pub fn dispatch_error_codec() -> codec.Codec(DispatchError) {
  codec.json_codec(
    fn(dispatch_error: DispatchError) {
      case dispatch_error {
        AssemblyRefused(message: message) ->
          json.object([
            #("error", json.string("assembly_refused")),
            #("message", json.string(message)),
          ])
        DispatchStageFailed(stage: stage, message: message) ->
          json.object([
            #("error", json.string("dispatch_stage_failed")),
            #("stage", json.string(stage)),
            #("message", json.string(message)),
          ])
      }
    },
    {
      use tag <- decode.field("error", decode.string)
      case tag {
        "assembly_refused" -> {
          use message <- decode.field("message", decode.string)
          decode.success(AssemblyRefused(message: message))
        }
        "dispatch_stage_failed" -> {
          use stage <- decode.field("stage", decode.string)
          use message <- decode.field("message", decode.string)
          decode.success(DispatchStageFailed(stage: stage, message: message))
        }
        _ ->
          decode.failure(
            AssemblyRefused(message: ""),
            "assembly_refused or dispatch_stage_failed",
          )
      }
    },
  )
}

/// Codec for the `dispatch_status` query reply.
pub fn dispatch_status_codec() -> codec.Codec(DispatchStatus) {
  codec.json_codec(
    fn(status: DispatchStatus) {
      json.object([
        #("current_brief", json.string(status.current_brief)),
        #("position", json.int(status.position)),
        #("total", json.int(status.total)),
        #("outcomes", json.array(status.outcomes, brief_outcome_to_json)),
      ])
    },
    {
      use current_brief <- decode.field("current_brief", decode.string)
      use position <- decode.field("position", decode.int)
      use total <- decode.field("total", decode.int)
      use outcomes <- decode.field(
        "outcomes",
        decode.list(brief_outcome_decoder()),
      )
      decode.success(DispatchStatus(
        current_brief: current_brief,
        position: position,
        total: total,
        outcomes: outcomes,
      ))
    },
  )
}

/// Codec for the `assemble_wave` activity input.
pub fn assemble_input_codec() -> codec.Codec(AssembleInput) {
  codec.json_codec(
    fn(input: AssembleInput) {
      json.object([
        #("design_dir", json.string(input.design_dir)),
        #("wave", json.array(input.wave, json.string)),
      ])
    },
    {
      use design_dir <- decode.field("design_dir", decode.string)
      use wave <- decode.field("wave", decode.list(decode.string))
      decode.success(AssembleInput(design_dir: design_dir, wave: wave))
    },
  )
}

/// Codec for the `assemble_wave` activity output: the ordered wave entries,
/// each reusing BD-001's brief document and resolved-context codecs.
pub fn assembled_wave_codec() -> codec.Codec(AssembledWave) {
  codec.json_codec(
    fn(wave: AssembledWave) {
      json.object([#("entries", json.array(wave.entries, wave_entry_to_json))])
    },
    {
      use entries <- decode.field("entries", decode.list(wave_entry_decoder()))
      decode.success(AssembledWave(entries: entries))
    },
  )
}

fn wave_entry_to_json(entry: WaveEntry) -> json.Json {
  json.object([
    #(
      "brief_document",
      codecs_brief.brief_document_to_json(entry.brief_document),
    ),
    #(
      "resolved_context",
      codecs_brief.resolved_context_to_json(entry.resolved_context),
    ),
  ])
}

fn wave_entry_decoder() -> decode.Decoder(WaveEntry) {
  use brief_document <- decode.field(
    "brief_document",
    codecs_brief.brief_document_decoder(),
  )
  use resolved_context <- decode.field(
    "resolved_context",
    codecs_brief.resolved_context_decoder(),
  )
  decode.success(WaveEntry(
    brief_document: brief_document,
    resolved_context: resolved_context,
  ))
}

/// Decoder for a `Placement` wire string, mirroring
/// `codecs_core.placement_to_string`.
fn placement_decoder() -> decode.Decoder(Placement) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "local" -> decode.success(Local)
      "remote" -> decode.success(Remote)
      _ -> decode.failure(Local, "local or remote")
    }
  })
}

/// Decoder for an `Isolation` wire string, mirroring
/// `codecs_core.isolation_to_string`.
fn isolation_decoder() -> decode.Decoder(Isolation) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "worktree" -> decode.success(Worktree)
      "copy" -> decode.success(Copy)
      "overlay" -> decode.success(Overlay)
      "vm" -> decode.success(Vm)
      _ -> decode.failure(Worktree, "worktree, copy, overlay, or vm")
    }
  })
}
