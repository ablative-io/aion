//// Stage prompt projection functions (P2): every stage prompt is a pure,
//// budgeted function of the brief document, the pre-resolved reference
//// context, and the explicit stage inputs it is handed — no filesystem
//// access, no ambient state, no engine imports. Projections are
//// unit-tested like codecs against the checklist budgets (scout 6000,
//// dev 9000, review 12000 characters).
////
//// Rendering discipline (PROMPTING.md):
//// - anchored decisions render as `id: title — decision` lines; when an
////   ADR carries a quote, the speaker's quoted words follow verbatim,
////   never paraphrased (P6);
//// - requirements inline their resolved C#/S# texts; the full checklist
////   and stories documents are never forwarded;
//// - every stage sees the brief's boundaries verbatim and the design file
////   as a path reference; design prose beyond the context's intention and
////   constraint lines never rides a prompt.
////
//// No ledger text lives in this module: quotes and decision texts flow
//// from `ResolvedContext` values at call time.

import aion_stacked_dev_io as stage_io
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list
import gleam/string
import stacked_dev/types.{
  type BriefDocument, type BriefRequirement, type CheckResult,
  type RequirementFiles, type ResolvedAdr, type ResolvedContext,
  type ResolvedItem, CheckFail, CheckPass,
}

/// Static scout-stage instructions: read-only orientation, not cataloguing.
pub const scout_instructions = "Scout this brief — read-only: explore the "
  <> "repository, change nothing. Per requirement: 2-5 files with line "
  <> "ranges the dev must read, the conventions the surrounding code "
  <> "follows, a concrete approach, the gotchas that would cost the dev "
  <> "time. Do not catalogue the codebase. Return a scout report against "
  <> "the scout-report schema, one entry per requirement."

/// Static dev-stage instructions: implement every R#, declare deviations,
/// leave the authoritative gate to the workflow.
pub const dev_instructions = "Implement this brief. Deliver every "
  <> "requirement exactly as specified, starting from the scout findings "
  <> "rendered under each one. If you deviate from the scouted approach, "
  <> "declare it in the dev report's deviation field for that requirement "
  <> "— silent deviation is a review finding. Use cargo check, cargo test, "
  <> "cargo clippy, and cargo fmt to verify your work compiles and passes "
  <> "before returning. Stay inside the boundaries. Return a dev report "
  <> "against the dev-report schema with one entry per requirement and an "
  <> "honest attestation."

/// Static review-stage instructions: verify the diff, evidence per
/// criterion, fix everything found.
pub const review_instructions = "Review this brief adversarially. Verify "
  <> "the actual diff, never the dev report — the report is a claim, the "
  <> "code is the evidence. Return one verdict per acceptance criterion "
  <> "with file:line or test-name evidence. Fix everything you find: there "
  <> "are no minor issues, and this review includes the harden pass. Use "
  <> "cargo check, cargo test, cargo clippy, and cargo fmt to verify your "
  <> "fixes compile and pass. Where the dev's attestation diverges from "
  <> "the measured checks, read the divergence as signal. Return a review "
  <> "report against the review-report schema."

/// The scout projection: instructions, binding decisions, provenance,
/// design context, every requirement with its resolved references, and the
/// boundaries. No enrichment blocks exist before the scout stage, so none
/// render.
pub fn scout_prompt(
  document: BriefDocument,
  context: ResolvedContext,
) -> String {
  join_blocks([
    orientation(document, context, scout_instructions),
    requirements_section(document.requirements, context, fn(_) { "" }),
    boundaries_section(document.boundaries),
  ])
}

/// The dev projection: everything the scout sees for orientation, plus the
/// scout's findings rendered inline under each requirement (C9).
pub fn dev_prompt(
  document: BriefDocument,
  context: ResolvedContext,
  scout: stage_io.ScoutReport,
) -> String {
  join_blocks([
    orientation(document, context, dev_instructions),
    requirements_section(document.requirements, context, fn(requirement) {
      scout_block(scout, requirement.id)
    }),
    boundaries_section(document.boundaries),
  ])
}

/// The review projection: per requirement the scout and dev blocks inline,
/// then the dev's attestation and the measured check results as two
/// distinct labelled sections — divergence between them is review signal
/// (P1) — plus the brief's verification steps and boundaries.
pub fn review_prompt(
  document: BriefDocument,
  context: ResolvedContext,
  dev: stage_io.DevReport,
  check: CheckResult,
) -> String {
  // The reviewer verifies the dev's diff against the brief with fresh eyes; it
  // is handed the brief, the dev record, the dev attestation, and the measured
  // checks — never the scout (that is the dev's orientation, not the reviewer's
  // input, and would only bias the verification). Decision: ADR-010.
  join_blocks([
    orientation(document, context, review_instructions),
    requirements_section(document.requirements, context, fn(requirement) {
      dev_block(dev, requirement.id)
    }),
    attestation_section(dev.attestation),
    measured_section(check),
    "Verification steps:\n" <> bulleted(document.verification),
    boundaries_section(document.boundaries),
  ])
}

/// The fix-round prompt: brief id, the wholesale-replacement instruction,
/// rendered diagnostics, and the boundaries. Requirements and scout
/// blocks are not re-rendered — the resumed session already holds them
/// (S9 context economy).
pub fn resume_feedback(document: BriefDocument, diagnostics: String) -> String {
  join_blocks([
    "Brief: " <> document.id,
    "Fix the reported failures below. Run the diagnostic commands after "
      <> "each fix to confirm. Then return a full replacement dev report "
      <> "against the dev-report schema. The replacement is wholesale — a "
      <> "complete report covering every requirement, never a partial field "
      <> "merge.",
    render_diagnostics(diagnostics),
    boundaries_section(document.boundaries),
  ])
}

/// Parse the scoped_checks diagnostics JSON and render human-readable
/// feedback: one block per failure with file, message, and fix guidance,
/// then de-duplicated do-not rules at the end.
fn render_diagnostics(raw: String) -> String {
  case json.parse(raw, diagnostics_report_decoder()) {
    Ok(report) -> format_report(report)
    Error(_) -> "Diagnostics:\n" <> raw
  }
}

type DiagnosticsReport {
  DiagnosticsReport(diagnostics: List(Diagnostic))
}

type Diagnostic {
  Diagnostic(
    file: String,
    line: Int,
    message: String,
    fix: String,
    do_not: List(String),
  )
}

fn diagnostics_report_decoder() -> decode.Decoder(DiagnosticsReport) {
  use diagnostics <- decode.field(
    "diagnostics",
    decode.list(diagnostic_decoder()),
  )
  decode.success(DiagnosticsReport(diagnostics:))
}

fn diagnostic_decoder() -> decode.Decoder(Diagnostic) {
  use #(file, line, message) <- decode.field("event", event_decoder())
  use #(fix, do_not) <- decode.field("verdict", verdict_decoder())
  decode.success(Diagnostic(file:, line:, message:, fix:, do_not:))
}

fn event_decoder() -> decode.Decoder(#(String, Int, String)) {
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use message <- decode.field("message", decode.string)
  decode.success(#(file, line, message))
}

fn verdict_decoder() -> decode.Decoder(#(String, List(String))) {
  decode.at(["Report", "guidance"], {
    use fix <- decode.field("fix", decode.string)
    use do_not <- decode.field("do_not", decode.list(decode.string))
    decode.success(#(fix, do_not))
  })
}

fn format_report(report: DiagnosticsReport) -> String {
  let failure_lines =
    list.map(report.diagnostics, fn(d) {
      "- " <> d.file <> ":" <> int.to_string(d.line) <> " — " <> d.message
      <> "\n  Fix: " <> d.fix
    })
  let all_do_nots =
    report.diagnostics
    |> list.flat_map(fn(d) { d.do_not })
    |> list.unique()
  let do_not_section = case all_do_nots {
    [] -> ""
    items ->
      "Do not:\n"
      <> {
        items
        |> list.map(fn(item) { "- " <> item })
        |> string.join("\n")
      }
  }
  join_blocks([
    "Failures (" <> int.to_string(list.length(report.diagnostics)) <> "):",
    string.join(failure_lines, "\n\n"),
    do_not_section,
  ])
}

/// One line per resolved ADR, in the order given, formatted exactly
/// `id: title — decision`. When the ADR's quote is non-empty, an
/// attribution line `decided_by: "quote"` follows immediately, verbatim;
/// when it is empty, no attribution line renders. ADR context and
/// consequences prose never render (budget discipline).
pub fn decision_context(adrs: List(ResolvedAdr)) -> String {
  adrs
  |> list.map(fn(adr) {
    let line = adr.id <> ": " <> adr.title <> " — " <> adr.decision
    case adr.quote {
      "" -> line
      quote -> line <> "\n" <> speaker_quote(adr.decided_by, quote)
    }
  })
  |> string.join("\n")
}

/// A speaker's verbatim words, quoted and attributed: `speaker: "quote"`.
/// Never paraphrased, truncated, or re-punctuated (P6).
pub fn speaker_quote(speaker: String, quote: String) -> String {
  speaker <> ": \"" <> quote <> "\""
}

/// The design-context body: the cluster intention, the resolved constraint
/// lines, and the design file as a path reference — never design.json
/// prose beyond these.
pub fn design_context(context: ResolvedContext) -> String {
  join_lines([
    context.intention,
    ..list.append(list.map(context.constraints, reference_line), [
      "Design file: " <> context.design_path,
    ])
  ])
}

/// One requirement rendered with id, title, spec, acceptance criteria,
/// files, and each of its C#/S# references inlined with its resolved text
/// from the context — never the full checklist or stories documents.
pub fn requirement_section(
  requirement: BriefRequirement,
  context: ResolvedContext,
) -> String {
  join_lines([
    requirement.id <> " — " <> requirement.title,
    "Spec: " <> requirement.spec,
    "Acceptance:",
    bulleted(requirement.acceptance),
    files_line(requirement.files),
    ..list.append(
      list.map(requirement.checklist, resolve(context.checklist, _)),
      list.map(requirement.stories, resolve(context.stories, _)),
    )
  ])
}

/// The brief's boundaries, verbatim, rendered in every stage prompt —
/// scope protection only works if no stage is exempt.
pub fn boundaries_section(boundaries: List(String)) -> String {
  "Boundaries:\n" <> bulleted(boundaries)
}

/// The scout's findings for one requirement, rendered inline under it. An
/// explicit `scout: none recorded` marker renders when the report carries
/// no entry for the requirement — the requirement itself is never omitted.
pub fn scout_block(
  report: stage_io.ScoutReport,
  requirement_id: String,
) -> String {
  case list.find(report.enrichments, fn(entry) { entry.id == requirement_id }) {
    Error(Nil) -> "scout: none recorded"
    Ok(entry) ->
      join_lines([
        "Scout findings:",
        "files: " <> string.join(entry.files, ", "),
        "context:",
        bulleted(entry.context),
        "approach: " <> entry.approach,
        "notes: " <> entry.notes,
      ])
  }
}

/// The dev's record for one requirement, rendered inline under it: status,
/// files changed, how, deviation, and the per-C#/S# claims. An explicit
/// `dev: none recorded` marker renders when the report carries no entry.
pub fn dev_block(report: stage_io.DevReport, requirement_id: String) -> String {
  case list.find(report.enrichments, fn(entry) { entry.id == requirement_id }) {
    Error(Nil) -> "dev: none recorded"
    Ok(entry) ->
      join_lines([
        "Dev record:",
        "status: " <> dev_status_text(entry.status),
        "files changed:",
        bulleted(list.map(entry.files_changed, file_change_line)),
        "how: " <> entry.how,
        "deviation: " <> entry.deviation,
        "checklist claims:",
        bulleted(list.map(entry.checklist, checklist_claim_line)),
        "story claims:",
        bulleted(list.map(entry.stories, story_claim_line)),
      ])
  }
}

/// The dev's attestation, labelled as the dev's claim — never presented as
/// a gate outcome (P1).
pub fn attestation_section(
  attestation: stage_io.DevReportAttestation,
) -> String {
  join_lines([
    "Dev attestation (the dev's claim, not a gate outcome):",
    "no_panics: " <> bool_text(attestation.no_panics),
    "no_unsafe: " <> bool_text(attestation.no_unsafe),
    "boundaries_respected: " <> bool_text(attestation.boundaries_respected),
    "tests_pass: " <> bool_text(attestation.tests_pass),
  ])
}

/// What the workflow measured: verdict, checked scope, and the diagnostics
/// when the checks failed — labelled measured, distinct from the
/// attestation, so the reviewer reads their divergence as signal (P1).
pub fn measured_section(check: CheckResult) -> String {
  let verdict_lines = case check.verdict {
    CheckPass -> ["verdict: pass", "checked_scope: " <> check.checked_scope]
    CheckFail(diagnostics) -> [
      "verdict: fail",
      "checked_scope: " <> check.checked_scope,
      "diagnostics:",
      diagnostics,
    ]
  }
  join_lines(["Measured checks (measured by the workflow):", ..verdict_lines])
}

fn orientation(
  document: BriefDocument,
  context: ResolvedContext,
  instructions: String,
) -> String {
  join_blocks([
    instructions,
    "Brief: "
      <> document.id
      <> " — "
      <> document.title
      <> " (cluster: "
      <> document.cluster
      <> ")",
    "Binding decisions:\n" <> decision_context(context.adrs),
    provenance_section(context),
    "Design context:\n" <> design_context(context),
  ])
}

fn provenance_section(context: ResolvedContext) -> String {
  case context.provenance.quote {
    "" -> ""
    quote ->
      "Provenance:\n" <> speaker_quote(context.provenance.requested_by, quote)
  }
}

fn requirements_section(
  requirements: List(BriefRequirement),
  context: ResolvedContext,
  stage_blocks: fn(BriefRequirement) -> String,
) -> String {
  let sections =
    list.map(requirements, fn(requirement) {
      join_lines([
        requirement_section(requirement, context),
        stage_blocks(requirement),
      ])
    })
  string.join(["Requirements:", ..sections], "\n\n")
}

/// Inline one C#/S# reference with its resolved text. A reference the
/// context does not carry renders a loud `unresolved in context` marker —
/// per CN1 the dispatcher resolves every reference before workflow logic
/// runs, so the marker only ever surfaces a dispatcher bug; the projection
/// stays total and never substitutes invented text.
fn resolve(items: List(ResolvedItem), id: String) -> String {
  case list.find(items, fn(item) { item.id == id }) {
    Ok(item) -> reference_line(item)
    Error(Nil) -> id <> " — unresolved in context"
  }
}

fn reference_line(item: ResolvedItem) -> String {
  item.id <> " — " <> item.text
}

fn files_line(files: RequirementFiles) -> String {
  "Files: create ["
  <> string.join(files.create, ", ")
  <> "] modify ["
  <> string.join(files.modify, ", ")
  <> "] delete ["
  <> string.join(files.delete, ", ")
  <> "]"
}

fn dev_status_text(status: stage_io.DevReportEnrichmentsItemStatus) -> String {
  case status {
    stage_io.DevReportEnrichmentsItemStatusImplemented -> "implemented"
    stage_io.DevReportEnrichmentsItemStatusBlocked -> "blocked"
  }
}

fn file_change_line(
  change: stage_io.DevReportEnrichmentsItemFilesChangedItem,
) -> String {
  change.path <> " (" <> change_kind_text(change.change) <> ") " <> change.note
}

fn change_kind_text(
  kind: stage_io.DevReportEnrichmentsItemFilesChangedItemChange,
) -> String {
  case kind {
    stage_io.DevReportEnrichmentsItemFilesChangedItemChangeCreated -> "created"
    stage_io.DevReportEnrichmentsItemFilesChangedItemChangeModified ->
      "modified"
    stage_io.DevReportEnrichmentsItemFilesChangedItemChangeDeleted -> "deleted"
  }
}

fn checklist_claim_line(
  claim: stage_io.DevReportEnrichmentsItemChecklistItem,
) -> String {
  claim.id <> " done: " <> bool_text(claim.done) <> " — " <> claim.note
}

fn story_claim_line(
  claim: stage_io.DevReportEnrichmentsItemStoriesItem,
) -> String {
  claim.id
  <> " satisfied: "
  <> bool_text(claim.satisfied)
  <> " — "
  <> claim.note
}

fn bool_text(value: Bool) -> String {
  case value {
    True -> "true"
    False -> "false"
  }
}

fn bulleted(items: List(String)) -> String {
  items
  |> list.map(fn(item) { "- " <> item })
  |> string.join("\n")
}

fn join_lines(lines: List(String)) -> String {
  lines
  |> list.filter(fn(line) { line != "" })
  |> string.join("\n")
}

fn join_blocks(blocks: List(String)) -> String {
  blocks
  |> list.filter(fn(block) { block != "" })
  |> string.join("\n\n")
}
