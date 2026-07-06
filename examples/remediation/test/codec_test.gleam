//// Codec contract tests. THE LOAD-BEARING ONE: the test-author input codec
//// must be structurally incapable of emitting a `recommendation` field — the
//// test-author independence rule (DESIGN.md Stage 1) enforced at the codec
//// layer, not by prompt discipline.

import gleam/option.{None, Some}
import gleam/string
import gleeunit/should
import remediation/codecs
import remediation/types.{
  Brief, ClassSibling, Correction, Deviation, FindingFix, FindingRuling,
  FixReport, Fixed, LedgerEntry, ManifestEntry, Partial, TestAuthorInput,
  TestManifest, Verdict,
}

/// A full ledger entry whose recommendation carries a sentinel value no other
/// field contains — if it ever appears in the encoded test-author input, the
/// strip has failed.
fn entry_with_secret_recommendation() -> types.LedgerEntry {
  LedgerEntry(
    id: "YG-268",
    title: "teardown deletes uncommitted work",
    file: "crates/yg-core/src/teardown.rs",
    line: 42,
    category: Correction,
    severity: "critical",
    detail: "the None-strategy teardown path lacks a dirty check",
    failure_scenario: "teardown with None strategy on a repo with uncommitted "
      <> "changes deletes them",
    recommendation: "SECRET-RECOMMENDATION-SENTINEL: add a dirty check before "
      <> "removal",
  )
}

fn brief() -> types.Brief {
  Brief(
    id: "B-1",
    finding_ids: ["YG-268", "YG-367"],
    root_cause: "the None-strategy teardown path lacks a dirty check and "
      <> "scope guard",
    files_expected: ["crates/yg-core/src/teardown.rs"],
    boundaries: ["crates/yg-cli"],
    acceptance: ["teardown refuses to delete uncommitted work"],
    wave: 0,
    deep_cluster: False,
  )
}

// --- MANDATORY: the codec strips the recommendation --------------------------

pub fn test_author_input_codec_cannot_emit_a_recommendation_test() {
  let input =
    TestAuthorInput(brief: brief(), entries: [
      types.strip_recommendation(entry_with_secret_recommendation()),
    ])
  let encoded = codecs.test_author_input_codec().encode(input)

  // Neither the field name nor the value can reach the wire: the encoder has
  // no recommendation field to emit.
  string.contains(encoded, "recommendation")
  |> should.equal(False)
  string.contains(encoded, "SECRET-RECOMMENDATION-SENTINEL")
  |> should.equal(False)
  // The entry itself did reach the wire (this is a strip, not a drop).
  string.contains(encoded, "YG-268")
  |> should.equal(True)
  string.contains(encoded, "failure_scenario")
  |> should.equal(True)
}

pub fn strip_recommendation_preserves_every_other_field_test() {
  let entry = entry_with_secret_recommendation()
  let stripped = types.strip_recommendation(entry)
  stripped.id |> should.equal(entry.id)
  stripped.title |> should.equal(entry.title)
  stripped.file |> should.equal(entry.file)
  stripped.line |> should.equal(entry.line)
  stripped.category |> should.equal(entry.category)
  stripped.severity |> should.equal(entry.severity)
  stripped.detail |> should.equal(entry.detail)
  stripped.failure_scenario |> should.equal(entry.failure_scenario)
}

pub fn developer_input_carries_the_full_recommendation_test() {
  // The developer DOES receive the recommendation (DESIGN.md Stage 2) — the
  // strip is test-author-specific, not global.
  let input =
    types.DeveloperInput(
      brief: brief(),
      entries: [entry_with_secret_recommendation()],
      manifest: manifest(),
      gate1_results: [],
      verdict: None,
      gate2: None,
    )
  let encoded = codecs.developer_input_codec().encode(input)
  string.contains(encoded, "SECRET-RECOMMENDATION-SENTINEL")
  |> should.equal(True)
}

// --- schema-shape round-trips -------------------------------------------------

fn manifest() -> types.TestManifest {
  TestManifest(brief_id: "B-1", entries: [
    ManifestEntry(
      finding_id: "YG-268",
      test_names: ["teardown::refuses_dirty_worktree"],
      fail_evidence: "assertion failed: teardown deleted uncommitted work",
      could_not_reproduce: False,
    ),
    ManifestEntry(
      finding_id: "YG-367",
      test_names: [],
      fail_evidence: "",
      could_not_reproduce: True,
    ),
  ])
}

pub fn test_manifest_codec_round_trips_test() {
  let codec = codecs.test_manifest_codec()
  codec.encode(manifest())
  |> codec.decode
  |> should.equal(Ok(manifest()))
}

pub fn test_manifest_decodes_the_schema_wire_shape_test() {
  let raw =
    "{\"brief_id\":\"B-1\",\"entries\":[{\"finding_id\":\"YG-268\","
    <> "\"test_names\":[\"t1\"],\"fail_evidence\":\"boom\","
    <> "\"could_not_reproduce\":false}]}"
  codecs.test_manifest_codec().decode(raw)
  |> should.equal(
    Ok(
      TestManifest(brief_id: "B-1", entries: [
        ManifestEntry(
          finding_id: "YG-268",
          test_names: ["t1"],
          fail_evidence: "boom",
          could_not_reproduce: False,
        ),
      ]),
    ),
  )
}

pub fn fix_report_codec_round_trips_test() {
  let report =
    FixReport(
      brief_id: "B-1",
      commits: ["abc123"],
      findings_addressed: [
        FindingFix(finding_id: "YG-268", how: "added the dirty check"),
      ],
      deviations: [
        Deviation(
          what: "touched cli",
          why: "shared helper",
          approved_by: "planner",
        ),
      ],
      new_tests: ["teardown::extra_case"],
    )
  let codec = codecs.fix_report_codec()
  codec.encode(report)
  |> codec.decode
  |> should.equal(Ok(report))
}

pub fn verdict_codec_round_trips_and_reads_ruling_tags_test() {
  let verdict =
    Verdict(
      brief_id: "B-1",
      per_finding: [
        FindingRuling(
          finding_id: "YG-268",
          ruling: Fixed,
          evidence: "read teardown.rs:42",
        ),
        FindingRuling(
          finding_id: "YG-367",
          ruling: Partial,
          evidence: "sibling at :189 survives",
        ),
      ],
      class_siblings_found: [
        ClassSibling(
          file: "sync.rs",
          line: 7,
          description: "same unguarded removal",
        ),
      ],
    )
  let codec = codecs.verdict_codec()
  codec.encode(verdict)
  |> codec.decode
  |> should.equal(Ok(verdict))

  string.contains(codec.encode(verdict), "\"ruling\":\"partial\"")
  |> should.equal(True)
}

pub fn brief_codec_preserves_the_schema_scope_nesting_test() {
  let encoded =
    codecs.brief_input_codec().encode(types.BriefInput(
      brief: brief(),
      entries: [entry_with_secret_recommendation()],
      config: config(),
    ))
  // The wire shape is the schema's nested scope, not the flattened Gleam
  // record.
  string.contains(encoded, "\"scope\":{\"files_expected\"")
  |> should.equal(True)

  codecs.brief_input_codec().decode(encoded)
  |> should.equal(
    Ok(types.BriefInput(
      brief: brief(),
      entries: [entry_with_secret_recommendation()],
      config: config(),
    )),
  )
}

fn config() -> types.RunConfig {
  types.RunConfig(
    repo_root: "/repo",
    ledger_path: "docs/reviews/audit.ledger.json",
    base_branch: "main",
    max_fix_cycles: 3,
  )
}

pub fn config_defaults_apply_when_absent_test() {
  let raw =
    "{\"brief\":"
    <> codec_brief_json()
    <> ",\"entries\":[],"
    <> "\"config\":{\"repo_root\":\"/repo\",\"ledger_path\":\"l.json\"}}"
  case codecs.brief_input_codec().decode(raw) {
    Ok(input) -> {
      input.config.base_branch |> should.equal("main")
      input.config.max_fix_cycles |> should.equal(3)
    }
    Error(_) -> should.fail()
  }
}

fn codec_brief_json() -> String {
  "{\"id\":\"B-1\",\"finding_ids\":[\"YG-268\"],\"root_cause\":\"rc\","
  <> "\"scope\":{\"files_expected\":[],\"boundaries\":[]},"
  <> "\"acceptance\":[\"a\"],\"wave\":0,\"deep_cluster\":false}"
}

pub fn ledger_entry_decoder_tolerates_full_schema_entries_test() {
  // A full ledger-entry.schema.json entry carries bookkeeping fields the
  // workflow does not read; they must be ignored, not fatal.
  let raw =
    "{\"brief\":"
    <> codec_brief_json()
    <> ",\"entries\":["
    <> "{\"id\":\"YG-268\",\"title\":\"t\",\"file\":\"f.rs\",\"line\":1,"
    <> "\"category\":\"correction\",\"severity\":\"high\",\"detail\":\"d\","
    <> "\"failure_scenario\":\"fs\",\"recommendation\":\"r\","
    <> "\"status\":\"briefed\",\"brief_id\":\"B-1\",\"test_names\":[],"
    <> "\"fix_commit\":null,\"verdict_ref\":null,\"evidence\":[],"
    <> "\"status_history\":[{\"status\":\"open\",\"timestamp\":\"t\","
    <> "\"actor\":\"a\"}],\"join_method\":\"title\",\"join_confidence\":\"exact\"}"
    <> "],\"config\":{\"repo_root\":\"/r\",\"ledger_path\":\"l\"}}"
  case codecs.brief_input_codec().decode(raw) {
    Ok(input) ->
      case input.entries {
        [entry] -> {
          entry.id |> should.equal("YG-268")
          entry.recommendation |> should.equal("r")
        }
        _ -> should.fail()
      }
    Error(_) -> should.fail()
  }
}

pub fn disposition_artifact_names_the_contracted_fields_test() {
  let artifact =
    codecs.disposition_artifact_json(
      brief_id: "B-1",
      disposition: types.CycleCapExhausted,
      fix_cycles: 3,
      test_edit_attempts: 1,
      could_not_reproduce: ["YG-367"],
      detail: "budget exhausted",
    )
  string.contains(artifact, "\"disposition\":\"cycle_cap_exhausted\"")
  |> should.equal(True)
  string.contains(artifact, "\"brief_id\":\"B-1\"")
  |> should.equal(True)
  string.contains(artifact, "\"could_not_reproduce\":[\"YG-367\"]")
  |> should.equal(True)
}

pub fn brief_result_codec_round_trips_with_optional_artifacts_test() {
  let result =
    types.BriefResult(
      brief_id: "B-1",
      disposition: types.Accepted,
      fix_cycles: 1,
      first_pass_accepted: True,
      could_not_reproduce: ["YG-367"],
      test_edit_attempts: 0,
      branch: "remediation/B-1",
      manifest: manifest(),
      fix_report: Some(
        FixReport(
          brief_id: "B-1",
          commits: ["abc"],
          findings_addressed: [FindingFix(finding_id: "YG-268", how: "fixed")],
          deviations: [],
          new_tests: [],
        ),
      ),
      verdict: None,
      ledger: [
        types.LedgerApplication(
          kind: "test_manifest",
          applied: True,
          detail: "ok",
        ),
      ],
      workspace_removed: True,
      summary: "accepted",
    )
  let codec = codecs.brief_result_codec()
  codec.encode(result)
  |> codec.decode
  |> should.equal(Ok(result))
}
