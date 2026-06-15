//// IO codec unit tests for the brief_dev workflow (BD-003 R7).
////
//// Pure codec tests: no network, no norn CLI, no workflow engine. The
//// BriefDevInput round-trip uses the seeded BD-001 fixture document (P4 — a
//// real authored contract, read, never written); the BriefDevResult
//// round-trip uses schema-valid ScoutReport/DevReport/ReviewReport values;
//// every BriefDevError variant is pinned to its exact encoded JSON; and
//// BriefDevStatus round-trips for each of the seven pipeline phases.

import aion_stacked_dev_io as stage_io
import gleam/list
import gleeunit/should
import stacked_dev/codecs_brief
import stacked_dev/codecs_workflows
import stacked_dev/types.{
  type BriefDevError, type ResolvedContext, type Workspace, BriefDevInput,
  BriefDevResult, BriefDevStageFailed, BriefDevStatus, BuildWarm, DevBlocked,
  DriftedRequirement, HardenRegressed, Local, ResolvedContext,
  ResolvedProvenance, ReviewDrifted, ScoutFailed, VerifyFixExhausted, Workspace,
  Worktree,
}
import support/fixtures

const fixture_path = "../../docs/design/brief-dev/briefs/BD-001.json"

fn fixture_document() -> types.BriefDocument {
  let assert Ok(raw) = fixtures.read_file(fixture_path)
  let assert Ok(document) = codecs_brief.brief_document_codec().decode(raw)
  document
}

fn sample_context() -> ResolvedContext {
  ResolvedContext(
    adrs: [],
    checklist: [],
    stories: [],
    constraints: [],
    intention: "Design-system v2 briefs become executable.",
    design_path: "docs/design/brief-dev/design.json",
    provenance: ResolvedProvenance(requested_by: "Tom", quote: "do this"),
  )
}

fn sample_workspace() -> Workspace {
  Workspace(
    path: "/w",
    branch: "stacked-dev-BD-001",
    placement: Local,
    isolation: Worktree,
  )
}

fn sample_scout_report() -> stage_io.ScoutReport {
  stage_io.ScoutReport(
    summary: "scouted",
    enrichments: [
      stage_io.ScoutReportEnrichmentsItem(
        id: "R1",
        files: ["src/a.gleam"],
        context: ["match conventions"],
        approach: "add it",
        notes: "",
      ),
    ],
    verification: ["gleam test"],
  )
}

fn sample_dev_report() -> stage_io.DevReport {
  stage_io.DevReport(
    summary: "implemented",
    commit_message: "feat: R1",
    enrichments: [
      stage_io.DevReportEnrichmentsItem(
        id: "R1",
        status: stage_io.DevReportEnrichmentsItemStatusImplemented,
        files_changed: [
          stage_io.DevReportEnrichmentsItemFilesChangedItem(
            path: "src/a.gleam",
            change: stage_io.DevReportEnrichmentsItemFilesChangedItemChangeModified,
            note: "added",
          ),
        ],
        how: "added it",
        deviation: "",
        checklist: [],
        stories: [],
      ),
    ],
    attestation: stage_io.DevReportAttestation(
      no_panics: True,
      no_unsafe: True,
      boundaries_respected: True,
      tests_pass: True,
    ),
  )
}

fn sample_review_report() -> stage_io.ReviewReport {
  stage_io.ReviewReport(
    summary: "verified",
    commit_message: "",
    enrichments: [
      stage_io.ReviewReportEnrichmentsItem(
        id: "R1",
        alignment: stage_io.ReviewReportEnrichmentsItemAlignmentAligned,
        acceptance: [
          stage_io.ReviewReportEnrichmentsItemAcceptanceItem(
            criterion: "it exists",
            met: True,
            evidence: "src/a.gleam:1",
          ),
        ],
        checklist: [],
        stories: [],
        issues: [],
        fixes: [],
      ),
    ],
    verification: [
      stage_io.ReviewReportVerificationItem(
        criterion: "gleam test",
        passed: True,
        note: "",
      ),
    ],
  )
}

pub fn brief_dev_input_round_trips_the_fixture_document_test() {
  let input =
    BriefDevInput(
      workspace: sample_workspace(),
      document: fixture_document(),
      context: sample_context(),
      verify_fix_cap: 3,
      round_backoff_ms: 25,
    )
  let input_codec = codecs_workflows.brief_dev_input_codec()
  input_codec.decode(input_codec.encode(input)) |> should.equal(Ok(input))
}

pub fn brief_dev_result_round_trips_with_all_three_reports_test() {
  let result =
    BriefDevResult(
      scout: sample_scout_report(),
      dev: sample_dev_report(),
      review: sample_review_report(),
      verify_rounds: 2,
      build_warm: BuildWarm(ok: True, duration_ms: 42),
    )
  let result_codec = codecs_workflows.brief_dev_result_codec()
  result_codec.decode(result_codec.encode(result)) |> should.equal(Ok(result))
}

fn pin(error_value: BriefDevError, expected: String) -> Nil {
  let error_codec = codecs_workflows.brief_dev_error_codec()
  error_codec.encode(error_value) |> should.equal(expected)
  error_codec.decode(expected) |> should.equal(Ok(error_value))
}

pub fn brief_dev_error_scout_failed_pins_test() {
  pin(
    ScoutFailed(message: "norn dead"),
    "{\"error\":\"scout_failed\",\"message\":\"norn dead\"}",
  )
}

pub fn brief_dev_error_dev_blocked_pins_test() {
  pin(
    DevBlocked(requirement_ids: ["R1", "R3"]),
    "{\"error\":\"dev_blocked\",\"requirement_ids\":[\"R1\",\"R3\"]}",
  )
}

pub fn brief_dev_error_verify_fix_exhausted_pins_test() {
  pin(
    VerifyFixExhausted(rounds: 3, diagnostics: "still failing"),
    "{\"error\":\"verify_fix_exhausted\",\"rounds\":3,\"diagnostics\":\"still failing\"}",
  )
}

pub fn brief_dev_error_review_drifted_pins_test() {
  pin(
    ReviewDrifted(drifted: [
      DriftedRequirement(id: "R2", issues: ["acceptance 3 unmet"]),
    ]),
    "{\"error\":\"review_drifted\",\"drifted\":[{\"id\":\"R2\",\"issues\":[\"acceptance 3 unmet\"]}]}",
  )
}

pub fn brief_dev_error_harden_regressed_pins_test() {
  pin(
    HardenRegressed(diagnostics: "diag"),
    "{\"error\":\"harden_regressed\",\"diagnostics\":\"diag\"}",
  )
}

pub fn brief_dev_error_stage_failed_pins_test() {
  pin(
    BriefDevStageFailed(stage: "startup", message: "boom"),
    "{\"error\":\"stage_failed\",\"stage\":\"startup\",\"message\":\"boom\"}",
  )
}

pub fn brief_dev_error_decode_rejects_unknown_tag_test() {
  codecs_workflows.brief_dev_error_codec().decode("{\"error\":\"bogus_tag\"}")
  |> should.be_error
}

pub fn brief_dev_status_round_trips_every_phase_test() {
  let status_codec = codecs_workflows.brief_dev_status_codec()
  [
    "scouting",
    "developing",
    "verifying",
    "fixing",
    "reviewing",
    "hardening",
    "converged",
  ]
  |> list.each(fn(phase) {
    let status = BriefDevStatus(phase: phase, round: 3)
    status_codec.decode(status_codec.encode(status))
    |> should.equal(Ok(status))
  })
}
