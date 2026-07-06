//// Per-run USER prompts for the four driven agent activities.
////
//// The ROLE DOCTRINE — the scout's grounding discipline, the planner's
//// decomposition rules, the dev's production-ready bar, the reviewer's
//// adversarial methodology — lives in the Rust worker's per-role SYSTEM prompt
//// (`--append-system-prompt`), fixed per role. What lives HERE is the
//// run-specific context each round: composed only from the STRUCTURED artifacts
//// upstream (the brief, the scout findings, the dev report, the review
//// verdict), never from an agent's prose summary of them (PIPELINE.md's rigid
//// composition rule). These functions are pure string assembly and are
//// deterministic — safe to run inside the workflow body.

import gleam/int
import gleam/list
import gleam/string
import pipeline_run/types.{
  type Blocker, type GateOutcome, type PipelineBrief, type ReviewVerdict,
  type ScoutFindings, type UnitInput,
}

fn section(title: String, body: String) -> String {
  "## " <> title <> "\n" <> body
}

fn bullets(items: List(String)) -> String {
  case items {
    [] -> "(none)"
    _ -> items |> list.map(fn(item) { "- " <> item }) |> string.join("\n")
  }
}

fn join(sections: List(String)) -> String {
  string.join(sections, "\n\n")
}

/// The brief contract block shared by scout and plan.
fn brief_contract(brief: PipelineBrief) -> String {
  join([
    section("Brief", brief.title <> " — " <> brief.intent),
    section("In scope", bullets(brief.scope_in)),
    section("Out of scope", bullets(brief.scope_out)),
    section("Acceptance criteria", bullets(brief.acceptance_criteria)),
    section("Constraints", bullets(brief.constraints)),
  ])
}

// --- scout -----------------------------------------------------------------

/// The scout USER prompt: ground the brief in the real tree at `repo_root`.
pub fn scout(brief: PipelineBrief) -> String {
  join([
    section(
      "Task",
      "Ground this brief in the actual repository at `"
        <> brief.repo_root
        <> "`. Read the real tree — do not work from description or memory. "
        <> "Report observed files and behavior with evidence, the integration "
        <> "points the work must respect, and the risks. Do not modify anything.",
    ),
    brief_contract(brief),
  ])
}

// --- plan ------------------------------------------------------------------

/// The plan USER prompt: decompose the brief into an ordered stack of units,
/// grounded in the scout findings.
pub fn plan(brief: PipelineBrief, findings: ScoutFindings) -> String {
  join([
    section(
      "Task",
      "Decompose this brief into a STACK of small units that can be developed "
        <> "and landed in dependency order. Each unit is an atomic, separately "
        <> "reviewable slice. Set `depends_on` to the unit ids whose landed work "
        <> "a unit must build on; units with no mutual dependency will be "
        <> "developed in parallel. Keep the graph acyclic. Prefer the smallest "
        <> "number of units that keeps each one coherent.",
    ),
    brief_contract(brief),
    section("Scout findings", findings.summary),
    section("Integration points", bullets(findings.integration_points)),
    section("Risks", bullets(findings.risks)),
  ])
}

// --- dev -------------------------------------------------------------------

/// The dev START prompt for a unit: implement this unit's goal, guided by the
/// brief contract and the scout grounding.
pub fn dev_start(unit: UnitInput) -> String {
  join([
    section(
      "Task",
      "Implement the following unit of the stack in this workspace. Produce "
        <> "complete, production-ready code: no partial work, no deferred TODOs, "
        <> "no silent failures. The workspace is already branched for this unit.",
    ),
    section("Unit", unit.unit_id <> ": " <> unit.goal),
    section("Files likely in play", bullets(unit.files_hint)),
    section("Brief", unit.brief_title <> " — " <> unit.brief_intent),
    section("Acceptance criteria", bullets(unit.acceptance_criteria)),
    section("Constraints", bullets(unit.constraints)),
    section("Scout grounding", unit.scout_summary),
  ])
}

/// The dev RESUME prompt after a failing review: address every blocker. The
/// driven norn session is resumed, so this need only carry the new findings.
pub fn dev_after_review(verdict: ReviewVerdict) -> String {
  join([
    section(
      "Review found blockers",
      "Address every blocker below, then report the files you changed and what "
        <> "you did. Do not defer any of them.",
    ),
    section("Blockers", bullets(blocker_lines(verdict.blockers))),
    section("Should fix", bullets(verdict.should_fix)),
    section("Reviewer summary", verdict.summary),
  ])
}

/// The dev RESUME prompt after a failing gate: fix what the cargo gate reported.
pub fn dev_after_gate(gate: GateOutcome) -> String {
  join([
    section(
      "The cargo gate failed",
      "`cargo clippy --all-targets -- -D warnings` then `cargo test` did not "
        <> "both pass. Fix every reported problem, then report your changes.",
    ),
    section("Gate diagnostics", gate.diagnostics),
  ])
}

// --- review ----------------------------------------------------------------

/// The review START prompt: adversarially review the dev work against the
/// unit's contract. The methodology is in the reviewer's system prompt.
pub fn review_start(unit: UnitInput, dev_summary: String) -> String {
  join([
    section(
      "Review this unit",
      "Adversarially review the implementation against the contract below — the "
        <> "brief, the unit goal, and the acceptance criteria — not merely the "
        <> "diff. Require file:line evidence for every blocker. `pass` is true "
        <> "ONLY with zero blockers and production-ready work.",
    ),
    section("Unit", unit.unit_id <> ": " <> unit.goal),
    section("Brief", unit.brief_title <> " — " <> unit.brief_intent),
    section("Acceptance criteria", bullets(unit.acceptance_criteria)),
    section("Constraints", bullets(unit.constraints)),
    section("Dev report", dev_summary),
  ])
}

/// The review RESUME prompt: re-review the corrected work with the same rigor.
pub fn review_resume(dev_summary: String) -> String {
  join([
    section(
      "Re-review",
      "Dev addressed the previous findings. Re-review with the same rigor and "
        <> "the same contract; do not pass unless every blocker is genuinely "
        <> "resolved and the work is production-ready.",
    ),
    section("Dev report", dev_summary),
  ])
}

// --- helpers ----------------------------------------------------------------

/// Render each blocker as a single `evidence — problem (scenario)` line.
fn blocker_lines(blockers: List(Blocker)) -> List(String) {
  list.map(blockers, fn(blocker) {
    blocker.evidence
    <> " — "
    <> blocker.problem
    <> " ("
    <> blocker.scenario
    <> ")"
  })
}

/// A one-line human rendering of a round counter, for summaries.
pub fn rounds_phrase(dev_review_rounds: Int, gate_rounds: Int) -> String {
  "dev<->review rounds: "
  <> int.to_string(dev_review_rounds)
  <> "; gate rounds: "
  <> int.to_string(gate_rounds)
}
