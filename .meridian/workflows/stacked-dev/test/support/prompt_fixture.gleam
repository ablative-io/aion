//// Canonical projection fixture shared by the four prompt test modules
//// (render, scout, dev, review) and the resume suite.
////
//// The fixture document derives from the SEEDED brief at
//// `docs/design/brief-dev/briefs/BD-001.json`, loaded through
//// `support/fixtures` and decoded with `codecs_brief`, so the projection
//// pins ride real authored contract text (P4).
////
//// DECLARED DEVIATION (BD-002): the full BD-001 document cannot satisfy
//// the checklist budgets — its six requirement specs plus acceptance
//// criteria alone total ~12,600 characters, above even the review budget
//// of 12,000 — so the fixture document keeps the decoded brief's authored
//// strings verbatim but takes deterministic subsets: the first two
//// requirements (R1, R2) and the first boundary. Every projection rule is
//// exercised against real authored text; nothing is paraphrased.
////
//// The resolved context carries the verbatim ADR-002 and ADR-007 ledger
//// texts and the verbatim RM-017 provenance quote attributed to Tom (P6).
//// The scout/dev/check values are constructed schema-valid values: no
//// captured real norn envelope exists yet for the v2 stage contracts —
//// P4's captured-real-envelope rule binds BD-007's CLI shims, not these
//// pure-function fixtures (declared in the BD-002 task).

import aion_stacked_dev_io as stage_io
import gleam/list
import stacked_dev/codecs_brief
import stacked_dev/types.{
  type BriefDocument, type CheckResult, type ResolvedContext, BriefDocument,
  CheckPass, CheckResult, ResolvedAdr, ResolvedContext, ResolvedItem,
  ResolvedProvenance,
}
import support/fixtures

const fixture_path = "../../docs/design/brief-dev/briefs/BD-001.json"

/// The fixture brief document: the decoded seeded BD-001 brief reduced to
/// its first two requirements and first boundary (see the module doc's
/// declared deviation). Authored strings are verbatim.
pub fn fixture_document() -> BriefDocument {
  let assert Ok(raw) = fixtures.read_file(fixture_path)
  let assert Ok(decoded) = codecs_brief.brief_document_codec().decode(raw)
  BriefDocument(
    ..decoded,
    requirements: list.take(decoded.requirements, 2),
    boundaries: list.take(decoded.boundaries, 1),
  )
}

/// The fixture resolved context: BD-001's design anchor (ADR-002, ADR-007)
/// with the ledger texts verbatim from docs/design/decisions.json, the
/// resolved C1/C2/S5 texts the fixture requirements reference, the CN7
/// constraint, and the RM-017 provenance quote verbatim from
/// docs/design/roadmap.json, attributed to Tom.
pub fn fixture_context() -> ResolvedContext {
  ResolvedContext(
    adrs: [
      ResolvedAdr(
        id: "ADR-002",
        title: "No backwards compatibility during the build",
        decision: "Replace, don't add alongside. No compat shims, no zombie "
          <> "code, no #[deprecated] markers. Breaking changes are made "
          <> "cleanly and consumers move forward. Rejected: incremental "
          <> "deprecation cycles — they double the surface under test for an "
          <> "audience of zero.",
        quote: "",
        decided_by: "Tom",
      ),
      ResolvedAdr(
        id: "ADR-007",
        title: "Design system v2: JSON ledgers above clusters, enrichment "
          <> "in place",
        decision: "Two project ledgers (roadmap.json, decisions.json) above "
          <> "the clusters; stage contracts as first-class schemas inside the "
          <> "aion codegen subset; the brief is one living document — the "
          <> "pipeline appends scout/dev/review per requirement and an "
          <> "execution block per brief, in place, never touching authored "
          <> "fields. Rejected: a sibling runs/ ledger — the brief as a "
          <> "single spec-plus-record document was the original intent, and "
          <> "aion's event history already provides the append-only audit "
          <> "trail.",
        quote: "basically stuff is just depended to it so I would actually "
          <> "be happy to have it to have it saved back in place from where "
          <> "it came from",
        decided_by: "Tom",
      ),
    ],
    checklist: [
      ResolvedItem(
        id: "C1",
        text: "schemas/scout_report.json, dev_report.json, and "
          <> "review_report.json exist in the package and are byte-identical "
          <> "to their docs/design-system/schemas/ canon files.",
      ),
      ResolvedItem(
        id: "C2",
        text: "scripts/check-schema-drift.sh fails loudly when any package "
          <> "stage-contract schema diverges from canon, and runs as part of "
          <> "the test gate.",
      ),
    ],
    stories: [
      ResolvedItem(
        id: "S5",
        text: "As Claude, I want stage payloads validated against the same "
          <> "schemas I author briefs against so that a malformed document "
          <> "breaks at dispatch with a pointer, not mid-run in an agent "
          <> "prompt.",
      ),
    ],
    constraints: [
      ResolvedItem(
        id: "CN7",
        text: "Package stage-contract schemas are byte-identical to "
          <> "docs/design-system/schemas/ canon, enforced by a drift gate "
          <> "that runs with the test suite.",
      ),
    ],
    intention: "Design-system v2 briefs become executable: an all-norn "
      <> "pipeline runs as a durable aion workflow, enriching the brief in "
      <> "place at every stage.",
    design_path: "docs/design/brief-dev/design.json",
    provenance: ResolvedProvenance(
      requested_by: "Tom",
      quote: "I would love it if you could write a workflow that that were "
        <> "not just right I mean write a workflow that sort of handles the "
        <> "pattern so goes through like and we try to do it all as much as "
        <> "we could with norn via the workflow.",
    ),
  )
}

/// A constructed schema-valid scout report with one enrichment per fixture
/// requirement. The approach strings carry sentinels the dev and review
/// projection tests assert on.
pub fn fixture_scout() -> stage_io.ScoutReport {
  stage_io.ScoutReport(
    summary: "Scouted both fixture requirements against the canon schemas.",
    enrichments: [
      stage_io.ScoutReportEnrichmentsItem(
        id: "R1",
        files: [
          "docs/design-system/schemas/scout-report.schema.json:1-40",
          "examples/stacked-dev/schemas/input.json:1-20",
        ],
        context: [
          "Canon schema filenames use hyphens and a .schema.json suffix.",
        ],
        approach: "FIXTURE-SCOUT-APPROACH-R1: copy the three canon files "
          <> "byte-for-byte under the snake_case rename mapping.",
        notes: "Only the filename changes; every content byte is preserved.",
      ),
      stage_io.ScoutReportEnrichmentsItem(
        id: "R2",
        files: ["examples/stacked-dev/scripts/:1-1"],
        context: ["The package has no scripts/ directory yet."],
        approach: "FIXTURE-SCOUT-APPROACH-R2: cmp each pair from the "
          <> "script's own directory so any cwd works.",
        notes: "Print every drifted or missing package schema by name.",
      ),
    ],
    verification: ["diff exits 0 for each copied schema pair."],
  )
}

/// A constructed schema-valid dev report with one enrichment per fixture
/// requirement and an all-true attestation. The deviation strings carry
/// sentinels the review projection test asserts on.
pub fn fixture_dev() -> stage_io.DevReport {
  stage_io.DevReport(
    summary: "Implemented both fixture requirements.",
    commit_message: "Copy stage-contract schemas and add the drift gate",
    enrichments: [
      stage_io.DevReportEnrichmentsItem(
        id: "R1",
        status: stage_io.DevReportEnrichmentsItemStatusImplemented,
        files_changed: [
          stage_io.DevReportEnrichmentsItemFilesChangedItem(
            path: "examples/stacked-dev/schemas/scout_report.json",
            change: stage_io.DevReportEnrichmentsItemFilesChangedItemChangeCreated,
            note: "byte-identical canon copy",
          ),
        ],
        how: "Copied the three canon files under the rename mapping.",
        deviation: "FIXTURE-DEV-DEVIATION-R1: none beyond the scouted plan.",
        checklist: [
          stage_io.DevReportEnrichmentsItemChecklistItem(
            id: "C1",
            done: True,
            note: "copies are byte-identical to canon",
          ),
        ],
        stories: [
          stage_io.DevReportEnrichmentsItemStoriesItem(
            id: "S5",
            satisfied: True,
            note: "payloads validate against the canon schemas",
          ),
        ],
      ),
      stage_io.DevReportEnrichmentsItem(
        id: "R2",
        status: stage_io.DevReportEnrichmentsItemStatusImplemented,
        files_changed: [
          stage_io.DevReportEnrichmentsItemFilesChangedItem(
            path: "examples/stacked-dev/scripts/check-schema-drift.sh",
            change: stage_io.DevReportEnrichmentsItemFilesChangedItemChangeCreated,
            note: "drift gate over the fixed rename mapping",
          ),
        ],
        how: "cmp per pair, paths resolved from the script location.",
        deviation: "FIXTURE-DEV-DEVIATION-R2: none.",
        checklist: [
          stage_io.DevReportEnrichmentsItemChecklistItem(
            id: "C2",
            done: True,
            note: "gate fails loudly on drift",
          ),
        ],
        stories: [
          stage_io.DevReportEnrichmentsItemStoriesItem(
            id: "S5",
            satisfied: True,
            note: "drift breaks the suite, not a live run",
          ),
        ],
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

/// A constructed passing measured-check result.
pub fn fixture_check() -> CheckResult {
  CheckResult(
    verdict: CheckPass,
    affected_modules: ["stacked_dev"],
    checked_scope: "workspace-wide",
  )
}
