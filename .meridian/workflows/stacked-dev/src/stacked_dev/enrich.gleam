//// Pure append-only merge of stage reports and the execution block into the
//// brief document (ADR-007: one living document).
////
//// Each merge replaces the targeted stage's blocks WHOLESALE — never
//// field-by-field — and copies every authored field through verbatim: no
//// code path here builds an authored field from report data. The module
//// performs no IO, no FFI call, and no CLI invocation; the `enrich_brief`
//// activity in `stacked_dev/locals` owns the worktree file read/write
//// (ADR-009), and the authored-subset guard it enforces (CN3) is expressed
//// here as the pure `authored_divergence` comparison.

import aion_stacked_dev_io as stage_io
import gleam/int
import gleam/list
import gleam/option
import gleam/result
import stacked_dev/types.{
  type BriefDocument, type BriefRequirement, type ChangeKind, type DevStatus,
  type ExecutionBlock, AcceptanceVerdict, Aligned, Blocked, BriefDocument,
  BriefRequirement, ChecklistClaim, Created, Deleted, DevBlock, Drifted,
  FileChange, Fixed, Implemented, Modified, ReviewBlock, ScoutBlock, StoryClaim,
}

/// A merge that cannot be applied: the report named a requirement the brief
/// does not carry — a contract mismatch between the stage output and the
/// authored document, never silently dropped.
pub type EnrichError {
  UnknownRequirementId(id: String)
}

/// Render an `EnrichError` as a single diagnostic line.
pub fn describe(enrich_error: EnrichError) -> String {
  case enrich_error {
    UnknownRequirementId(id: id) ->
      "report enrichment names requirement "
      <> id
      <> ", which the brief does not define"
  }
}

/// Merge a scout report into the document: for each report entry, the
/// matching requirement's `scout` block becomes exactly that entry's fields,
/// replacing any existing scout block wholesale.
pub fn merge_scout(
  document: BriefDocument,
  report: stage_io.ScoutReport,
) -> Result(BriefDocument, EnrichError) {
  merge_entries(
    document,
    list.map(report.enrichments, fn(entry) {
      #(entry.id, fn(requirement) {
        BriefRequirement(..requirement, scout: option.Some(scout_block(entry)))
      })
    }),
  )
}

/// Merge a dev report into the document: for each report entry, the matching
/// requirement's `dev` block becomes exactly that entry's fields, replacing
/// any existing dev block wholesale.
pub fn merge_dev(
  document: BriefDocument,
  report: stage_io.DevReport,
) -> Result(BriefDocument, EnrichError) {
  merge_entries(
    document,
    list.map(report.enrichments, fn(entry) {
      #(entry.id, fn(requirement) {
        BriefRequirement(..requirement, dev: option.Some(dev_block(entry)))
      })
    }),
  )
}

/// Merge a review report into the document: for each report entry, the
/// matching requirement's `review` block becomes exactly that entry's
/// fields, replacing any existing review block wholesale.
pub fn merge_review(
  document: BriefDocument,
  report: stage_io.ReviewReport,
) -> Result(BriefDocument, EnrichError) {
  merge_entries(
    document,
    list.map(report.enrichments, fn(entry) {
      #(entry.id, fn(requirement) {
        BriefRequirement(
          ..requirement,
          review: option.Some(review_block(entry)),
        )
      })
    }),
  )
}

/// Merge an execution block into the document: the document's `execution`
/// field becomes exactly the handed block, replacing any existing one
/// wholesale. Returns `Result` for uniformity with the report merges.
pub fn merge_execution(
  document: BriefDocument,
  block: ExecutionBlock,
) -> Result(BriefDocument, EnrichError) {
  Ok(BriefDocument(..document, execution: option.Some(block)))
}

/// The document with every pipeline-appended block removed: exactly the
/// fields an author wrote. Encoding this projection before and after a merge
/// proves the merge never touched an authored field.
pub fn authored_subset(document: BriefDocument) -> BriefDocument {
  BriefDocument(
    ..document,
    requirements: list.map(document.requirements, fn(requirement) {
      BriefRequirement(
        ..requirement,
        scout: option.None,
        dev: option.None,
        review: option.None,
      )
    }),
    execution: option.None,
  )
}

/// The first authored field that differs between two documents, named in the
/// schema's property order (`requirements/2/spec` for a nested divergence),
/// or `None` when the authored subsets are identical. Enrichment blocks are
/// never compared — they are the pipeline's to change.
pub fn authored_divergence(
  disk: BriefDocument,
  handed: BriefDocument,
) -> option.Option(String) {
  first_failing([
    #("id", disk.id == handed.id),
    #("cluster", disk.cluster == handed.cluster),
    #("title", disk.title == handed.title),
    #("depends_on", disk.depends_on == handed.depends_on),
    #("blocked_by", disk.blocked_by == handed.blocked_by),
    #("checklist", disk.checklist == handed.checklist),
    #("stories", disk.stories == handed.stories),
    #("design_anchor", disk.design_anchor == handed.design_anchor),
    #("purpose", disk.purpose == handed.purpose),
    #("task", disk.task == handed.task),
  ])
  |> option.lazy_or(fn() {
    requirements_divergence(disk.requirements, handed.requirements)
  })
  |> option.lazy_or(fn() {
    first_failing([
      #("boundaries", disk.boundaries == handed.boundaries),
      #("verification", disk.verification == handed.verification),
    ])
  })
}

fn requirements_divergence(
  disk: List(BriefRequirement),
  handed: List(BriefRequirement),
) -> option.Option(String) {
  case list.length(disk) == list.length(handed) {
    False -> option.Some("requirements")
    True -> walk_requirements(disk, handed, 0)
  }
}

fn walk_requirements(
  disk: List(BriefRequirement),
  handed: List(BriefRequirement),
  index: Int,
) -> option.Option(String) {
  case disk, handed {
    [first_disk, ..rest_disk], [first_handed, ..rest_handed] ->
      case requirement_divergence(index, first_disk, first_handed) {
        option.Some(field) -> option.Some(field)
        option.None -> walk_requirements(rest_disk, rest_handed, index + 1)
      }
    _, _ -> option.None
  }
}

fn requirement_divergence(
  index: Int,
  disk: BriefRequirement,
  handed: BriefRequirement,
) -> option.Option(String) {
  let prefix = "requirements/" <> int.to_string(index) <> "/"
  first_failing([
    #(prefix <> "id", disk.id == handed.id),
    #(prefix <> "title", disk.title == handed.title),
    #(prefix <> "spec", disk.spec == handed.spec),
    #(prefix <> "acceptance", disk.acceptance == handed.acceptance),
    #(prefix <> "files", disk.files == handed.files),
    #(prefix <> "checklist", disk.checklist == handed.checklist),
    #(prefix <> "stories", disk.stories == handed.stories),
  ])
}

fn first_failing(checks: List(#(String, Bool))) -> option.Option(String) {
  checks
  |> list.find(fn(check) { !check.1 })
  |> result.map(fn(check) { check.0 })
  |> option.from_result
}

/// Apply every `#(requirement id, set block)` update, replacing that stage's
/// block wholesale on the matching requirement. An update naming an unknown
/// R# fails the whole merge — no partially merged document is ever returned.
fn merge_entries(
  document: BriefDocument,
  updates: List(#(String, fn(BriefRequirement) -> BriefRequirement)),
) -> Result(BriefDocument, EnrichError) {
  updates
  |> list.try_fold(document.requirements, replace_requirement)
  |> result.map(fn(requirements) {
    BriefDocument(..document, requirements: requirements)
  })
}

fn replace_requirement(
  requirements: List(BriefRequirement),
  update: #(String, fn(BriefRequirement) -> BriefRequirement),
) -> Result(List(BriefRequirement), EnrichError) {
  let #(id, set_block) = update
  case list.any(requirements, fn(requirement) { requirement.id == id }) {
    False -> Error(UnknownRequirementId(id: id))
    True ->
      Ok(
        list.map(requirements, fn(requirement) {
          case requirement.id == id {
            True -> set_block(requirement)
            False -> requirement
          }
        }),
      )
  }
}

fn scout_block(entry: stage_io.ScoutReportEnrichmentsItem) -> types.ScoutBlock {
  ScoutBlock(
    files: entry.files,
    context: entry.context,
    approach: entry.approach,
    notes: entry.notes,
  )
}

fn dev_block(entry: stage_io.DevReportEnrichmentsItem) -> types.DevBlock {
  DevBlock(
    status: dev_status(entry.status),
    files_changed: list.map(entry.files_changed, file_change),
    how: entry.how,
    deviation: entry.deviation,
    checklist: list.map(entry.checklist, fn(claim) {
      ChecklistClaim(id: claim.id, done: claim.done, note: claim.note)
    }),
    stories: list.map(entry.stories, fn(claim) {
      StoryClaim(id: claim.id, satisfied: claim.satisfied, note: claim.note)
    }),
  )
}

fn dev_status(status: stage_io.DevReportEnrichmentsItemStatus) -> DevStatus {
  case status {
    stage_io.DevReportEnrichmentsItemStatusImplemented -> Implemented
    stage_io.DevReportEnrichmentsItemStatusBlocked -> Blocked
  }
}

fn file_change(
  item: stage_io.DevReportEnrichmentsItemFilesChangedItem,
) -> types.FileChange {
  FileChange(path: item.path, change: change_kind(item.change), note: item.note)
}

fn change_kind(
  change: stage_io.DevReportEnrichmentsItemFilesChangedItemChange,
) -> ChangeKind {
  case change {
    stage_io.DevReportEnrichmentsItemFilesChangedItemChangeCreated -> Created
    stage_io.DevReportEnrichmentsItemFilesChangedItemChangeModified -> Modified
    stage_io.DevReportEnrichmentsItemFilesChangedItemChangeDeleted -> Deleted
  }
}

fn review_block(
  entry: stage_io.ReviewReportEnrichmentsItem,
) -> types.ReviewBlock {
  ReviewBlock(
    alignment: case entry.alignment {
      stage_io.ReviewReportEnrichmentsItemAlignmentAligned -> Aligned
      stage_io.ReviewReportEnrichmentsItemAlignmentDrifted -> Drifted
      stage_io.ReviewReportEnrichmentsItemAlignmentFixed -> Fixed
    },
    acceptance: list.map(entry.acceptance, fn(verdict) {
      AcceptanceVerdict(
        criterion: verdict.criterion,
        met: verdict.met,
        evidence: verdict.evidence,
      )
    }),
    checklist: entry.checklist,
    stories: entry.stories,
    issues: entry.issues,
    fixes: entry.fixes,
  )
}
