//// Hand-written JSON codecs for the brief enrichment blocks: the scout,
//// dev, and review blocks the pipeline appends per requirement, and the
//// execution block it appends per brief.
////
//// Wire shapes mirror `docs/design-system/schemas/brief.schema.json`
//// exactly — property names, enum strings, and required fields. Decoding
//// never substitutes a default for a missing field and never accepts an
//// enum string outside the schema's set. Document-level codecs live in
//// `stacked_dev/codecs_brief`; this module exists so both stay under the
//// module-size cap.

import gleam/dynamic/decode
import gleam/json
import stacked_dev/types.{
  type AcceptanceVerdict, type Alignment, type AttestationBlock, type ChangeKind,
  type ChecklistClaim, type DevBlock, type DevStatus, type ExecutionBlock,
  type ExecutionStatus, type ExecutionVerdict, type FileChange, type GateBlock,
  type ReviewBlock, type ScoutBlock, type StoryClaim, AcceptanceVerdict, Aligned,
  AttestationBlock, Blocked, ChecklistClaim, Created, Deleted, DevBlock, Drifted,
  ExecutionBlock, ExecutionFailed, ExecutionInFlight, ExecutionLanded,
  FileChange, Fixed, GateBlock, Implemented, Modified, ReviewBlock, ScoutBlock,
  StoryClaim, VerdictApproved, VerdictChangesRequested, VerdictRejected,
}

/// JSON encoder for a requirement's scout block.
pub fn scout_block_to_json(block: ScoutBlock) -> json.Json {
  json.object([
    #("files", json.array(block.files, json.string)),
    #("context", json.array(block.context, json.string)),
    #("approach", json.string(block.approach)),
    #("notes", json.string(block.notes)),
  ])
}

/// Decoder for a requirement's scout block.
pub fn scout_block_decoder() -> decode.Decoder(ScoutBlock) {
  use files <- decode.field("files", decode.list(decode.string))
  use context <- decode.field("context", decode.list(decode.string))
  use approach <- decode.field("approach", decode.string)
  use notes <- decode.field("notes", decode.string)
  decode.success(ScoutBlock(
    files: files,
    context: context,
    approach: approach,
    notes: notes,
  ))
}

/// JSON encoder for a requirement's dev block.
pub fn dev_block_to_json(block: DevBlock) -> json.Json {
  json.object([
    #("status", dev_status_to_json(block.status)),
    #("files_changed", json.array(block.files_changed, file_change_to_json)),
    #("how", json.string(block.how)),
    #("deviation", json.string(block.deviation)),
    #("checklist", json.array(block.checklist, checklist_claim_to_json)),
    #("stories", json.array(block.stories, story_claim_to_json)),
  ])
}

/// Decoder for a requirement's dev block.
pub fn dev_block_decoder() -> decode.Decoder(DevBlock) {
  use status <- decode.field("status", dev_status_decoder())
  use files_changed <- decode.field(
    "files_changed",
    decode.list(file_change_decoder()),
  )
  use how <- decode.field("how", decode.string)
  use deviation <- decode.field("deviation", decode.string)
  use checklist <- decode.field(
    "checklist",
    decode.list(checklist_claim_decoder()),
  )
  use stories <- decode.field("stories", decode.list(story_claim_decoder()))
  decode.success(DevBlock(
    status: status,
    files_changed: files_changed,
    how: how,
    deviation: deviation,
    checklist: checklist,
    stories: stories,
  ))
}

/// Wire encoding of a `DevStatus`: exactly `implemented` or `blocked`.
pub fn dev_status_to_json(status: DevStatus) -> json.Json {
  case status {
    Implemented -> json.string("implemented")
    Blocked -> json.string("blocked")
  }
}

/// Decoder for a `DevStatus`; any string outside the schema's enum fails.
pub fn dev_status_decoder() -> decode.Decoder(DevStatus) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "implemented" -> decode.success(Implemented)
      "blocked" -> decode.success(Blocked)
      _ -> decode.failure(Implemented, "implemented or blocked")
    }
  })
}

fn file_change_to_json(change: FileChange) -> json.Json {
  json.object([
    #("path", json.string(change.path)),
    #("change", change_kind_to_json(change.change)),
    #("note", json.string(change.note)),
  ])
}

fn file_change_decoder() -> decode.Decoder(FileChange) {
  use path <- decode.field("path", decode.string)
  use change <- decode.field("change", change_kind_decoder())
  use note <- decode.field("note", decode.string)
  decode.success(FileChange(path: path, change: change, note: note))
}

/// Wire encoding of a `ChangeKind`: exactly `created`, `modified`, or
/// `deleted`.
pub fn change_kind_to_json(kind: ChangeKind) -> json.Json {
  case kind {
    Created -> json.string("created")
    Modified -> json.string("modified")
    Deleted -> json.string("deleted")
  }
}

/// Decoder for a `ChangeKind`; any string outside the schema's enum fails.
pub fn change_kind_decoder() -> decode.Decoder(ChangeKind) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "created" -> decode.success(Created)
      "modified" -> decode.success(Modified)
      "deleted" -> decode.success(Deleted)
      _ -> decode.failure(Created, "created, modified, or deleted")
    }
  })
}

fn checklist_claim_to_json(claim: ChecklistClaim) -> json.Json {
  json.object([
    #("id", json.string(claim.id)),
    #("done", json.bool(claim.done)),
    #("note", json.string(claim.note)),
  ])
}

fn checklist_claim_decoder() -> decode.Decoder(ChecklistClaim) {
  use id <- decode.field("id", decode.string)
  use done <- decode.field("done", decode.bool)
  use note <- decode.field("note", decode.string)
  decode.success(ChecklistClaim(id: id, done: done, note: note))
}

fn story_claim_to_json(claim: StoryClaim) -> json.Json {
  json.object([
    #("id", json.string(claim.id)),
    #("satisfied", json.bool(claim.satisfied)),
    #("note", json.string(claim.note)),
  ])
}

fn story_claim_decoder() -> decode.Decoder(StoryClaim) {
  use id <- decode.field("id", decode.string)
  use satisfied <- decode.field("satisfied", decode.bool)
  use note <- decode.field("note", decode.string)
  decode.success(StoryClaim(id: id, satisfied: satisfied, note: note))
}

/// JSON encoder for a requirement's review block.
pub fn review_block_to_json(block: ReviewBlock) -> json.Json {
  json.object([
    #("alignment", alignment_to_json(block.alignment)),
    #("acceptance", json.array(block.acceptance, acceptance_verdict_to_json)),
    #("checklist", json.array(block.checklist, json.string)),
    #("stories", json.array(block.stories, json.string)),
    #("issues", json.array(block.issues, json.string)),
    #("fixes", json.array(block.fixes, json.string)),
  ])
}

/// Decoder for a requirement's review block.
pub fn review_block_decoder() -> decode.Decoder(ReviewBlock) {
  use alignment <- decode.field("alignment", alignment_decoder())
  use acceptance <- decode.field(
    "acceptance",
    decode.list(acceptance_verdict_decoder()),
  )
  use checklist <- decode.field("checklist", decode.list(decode.string))
  use stories <- decode.field("stories", decode.list(decode.string))
  use issues <- decode.field("issues", decode.list(decode.string))
  use fixes <- decode.field("fixes", decode.list(decode.string))
  decode.success(ReviewBlock(
    alignment: alignment,
    acceptance: acceptance,
    checklist: checklist,
    stories: stories,
    issues: issues,
    fixes: fixes,
  ))
}

/// Wire encoding of an `Alignment`: exactly `aligned`, `drifted`, or
/// `fixed`.
pub fn alignment_to_json(alignment: Alignment) -> json.Json {
  case alignment {
    Aligned -> json.string("aligned")
    Drifted -> json.string("drifted")
    Fixed -> json.string("fixed")
  }
}

/// Decoder for an `Alignment`; any string outside the schema's enum fails.
pub fn alignment_decoder() -> decode.Decoder(Alignment) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "aligned" -> decode.success(Aligned)
      "drifted" -> decode.success(Drifted)
      "fixed" -> decode.success(Fixed)
      _ -> decode.failure(Aligned, "aligned, drifted, or fixed")
    }
  })
}

fn acceptance_verdict_to_json(verdict: AcceptanceVerdict) -> json.Json {
  json.object([
    #("criterion", json.string(verdict.criterion)),
    #("met", json.bool(verdict.met)),
    #("evidence", json.string(verdict.evidence)),
  ])
}

fn acceptance_verdict_decoder() -> decode.Decoder(AcceptanceVerdict) {
  use criterion <- decode.field("criterion", decode.string)
  use met <- decode.field("met", decode.bool)
  use evidence <- decode.field("evidence", decode.string)
  decode.success(AcceptanceVerdict(
    criterion: criterion,
    met: met,
    evidence: evidence,
  ))
}

/// JSON encoder for a brief's execution block.
pub fn execution_block_to_json(block: ExecutionBlock) -> json.Json {
  json.object([
    #("status", execution_status_to_json(block.status)),
    #("workflow_id", json.string(block.workflow_id)),
    #("branch", json.string(block.branch)),
    #("session_id", json.string(block.session_id)),
    #("gate", gate_block_to_json(block.gate)),
    #("attestation", attestation_block_to_json(block.attestation)),
    #("review_verdict", execution_verdict_to_json(block.review_verdict)),
    #("landed_commit", json.string(block.landed_commit)),
    #("merged_into", json.string(block.merged_into)),
    #("completed_at", json.string(block.completed_at)),
  ])
}

/// Decoder for a brief's execution block.
pub fn execution_block_decoder() -> decode.Decoder(ExecutionBlock) {
  use status <- decode.field("status", execution_status_decoder())
  use workflow_id <- decode.field("workflow_id", decode.string)
  use branch <- decode.field("branch", decode.string)
  use session_id <- decode.field("session_id", decode.string)
  use gate <- decode.field("gate", gate_block_decoder())
  use attestation <- decode.field("attestation", attestation_block_decoder())
  use review_verdict <- decode.field(
    "review_verdict",
    execution_verdict_decoder(),
  )
  use landed_commit <- decode.field("landed_commit", decode.string)
  use merged_into <- decode.field("merged_into", decode.string)
  use completed_at <- decode.field("completed_at", decode.string)
  decode.success(ExecutionBlock(
    status: status,
    workflow_id: workflow_id,
    branch: branch,
    session_id: session_id,
    gate: gate,
    attestation: attestation,
    review_verdict: review_verdict,
    landed_commit: landed_commit,
    merged_into: merged_into,
    completed_at: completed_at,
  ))
}

/// Wire encoding of an `ExecutionStatus`: exactly `in_flight`, `landed`, or
/// `failed`.
pub fn execution_status_to_json(status: ExecutionStatus) -> json.Json {
  case status {
    ExecutionInFlight -> json.string("in_flight")
    ExecutionLanded -> json.string("landed")
    ExecutionFailed -> json.string("failed")
  }
}

/// Decoder for an `ExecutionStatus`; any string outside the schema's enum
/// fails.
pub fn execution_status_decoder() -> decode.Decoder(ExecutionStatus) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "in_flight" -> decode.success(ExecutionInFlight)
      "landed" -> decode.success(ExecutionLanded)
      "failed" -> decode.success(ExecutionFailed)
      _ -> decode.failure(ExecutionInFlight, "in_flight, landed, or failed")
    }
  })
}

/// Wire encoding of an `ExecutionVerdict`: exactly `approved`,
/// `changes_requested`, or `rejected`.
pub fn execution_verdict_to_json(verdict: ExecutionVerdict) -> json.Json {
  case verdict {
    VerdictApproved -> json.string("approved")
    VerdictChangesRequested -> json.string("changes_requested")
    VerdictRejected -> json.string("rejected")
  }
}

/// Decoder for an `ExecutionVerdict`; any string outside the schema's enum
/// fails.
pub fn execution_verdict_decoder() -> decode.Decoder(ExecutionVerdict) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "approved" -> decode.success(VerdictApproved)
      "changes_requested" -> decode.success(VerdictChangesRequested)
      "rejected" -> decode.success(VerdictRejected)
      _ ->
        decode.failure(
          VerdictApproved,
          "approved, changes_requested, or rejected",
        )
    }
  })
}

fn gate_block_to_json(block: GateBlock) -> json.Json {
  json.object([
    #("fmt", json.bool(block.fmt)),
    #("clippy", json.bool(block.clippy)),
    #("tests", json.bool(block.tests)),
    #("fix_rounds", json.int(block.fix_rounds)),
  ])
}

fn gate_block_decoder() -> decode.Decoder(GateBlock) {
  use fmt <- decode.field("fmt", decode.bool)
  use clippy <- decode.field("clippy", decode.bool)
  use tests <- decode.field("tests", decode.bool)
  use fix_rounds <- decode.field("fix_rounds", decode.int)
  decode.success(GateBlock(
    fmt: fmt,
    clippy: clippy,
    tests: tests,
    fix_rounds: fix_rounds,
  ))
}

fn attestation_block_to_json(block: AttestationBlock) -> json.Json {
  json.object([
    #("no_panics", json.bool(block.no_panics)),
    #("no_unsafe", json.bool(block.no_unsafe)),
    #("boundaries_respected", json.bool(block.boundaries_respected)),
    #("tests_pass", json.bool(block.tests_pass)),
  ])
}

fn attestation_block_decoder() -> decode.Decoder(AttestationBlock) {
  use no_panics <- decode.field("no_panics", decode.bool)
  use no_unsafe <- decode.field("no_unsafe", decode.bool)
  use boundaries_respected <- decode.field("boundaries_respected", decode.bool)
  use tests_pass <- decode.field("tests_pass", decode.bool)
  decode.success(AttestationBlock(
    no_panics: no_panics,
    no_unsafe: no_unsafe,
    boundaries_respected: boundaries_respected,
    tests_pass: tests_pass,
  ))
}
