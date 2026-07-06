//// The JSON codecs binding every remediation type to its wire shape.
////
//// Three kinds of contract live here:
////
//// 1. The workflow input decoders (`BriefInput`, `WaveInput`) — the schema
////    wire shapes (`brief.schema.json` scope nesting preserved), tolerant of
////    the extra bookkeeping fields a full ledger entry carries, defaulting
////    the overridable config values.
//// 2. The three AGENT output codecs (`TestManifest`/`FixReport`/`Verdict`) —
////    the Gleam mirror of the copied yggdrasil schemas the driven harnesses
////    enforce via `--output-schema`.
//// 3. The SHELL activity wire codecs plus the child/parent results — each
////    byte-compatible with the Rust worker's serde types
////    (`worker/src/types.rs`).
////
//// THE INDEPENDENCE GUARANTEE LIVES HERE TOO: `test_author_input_codec`
//// encodes [`TestAuthorEntry`] values, a type with no `recommendation` field
//// — there is no code path by which a recommendation can appear in the
//// test-author's activity input. `test/codec_test.gleam` pins this.

import aion/codec.{type Codec}
import gleam/dynamic/decode
import gleam/json
import gleam/option.{type Option}
import remediation/types.{
  type AcceptanceCheck, type ArtifactKind, type Brief, type BriefInput,
  type BriefResult, type Category, type ClassInstance, type ClassSibling,
  type CleanupInput, type CleanupOutcome, type DeveloperInput, type Deviation,
  type Disposition, type FindingBounce, type FindingFix, type FindingRuling,
  type FixMetrics, type FixReport, type FlowMetrics, type Gate1Check,
  type Gate1Input, type Gate1Outcome, type Gate2Input, type Gate2Outcome,
  type LedgerApplication, type LedgerEntry, type LedgerUpdateInput,
  type LedgerUpdateOutcome, type ManifestEntry, type Overall,
  type ProvisionInput, type ReAuditMetrics, type RegressionRisk,
  type RemediationError, type Ruling, type RunConfig, type TestAuthorEntry,
  type TestAuthorInput, type TestAuthoringMetrics, type TestManifest,
  type TestRun, type Verdict, type VerifierInput, type VerifyMetrics,
  type WaveBrief, type WaveInput, type WaveMetrics, type WaveReport,
  type WaveResult, type WorkspaceInfo, AcceptanceCheck, Brief, BriefInput,
  BriefResult, ChildFailed, ClassInstance, ClassSibling, CleanupInput,
  CleanupOutcome, DecodeInputFailed, DeveloperInput, Deviation, FindingBounce,
  FindingFix, FindingRuling, FixMetrics, FixReport, FlowMetrics, Gate1Check,
  Gate1Input, Gate1Outcome, Gate2Input, Gate2Outcome, LedgerApplication,
  LedgerEntry, LedgerUpdateInput, LedgerUpdateOutcome, ManifestEntry,
  ProvisionInput, ReAuditMetrics, RegressionRisk, RunConfig, StageFailed,
  StrataInvalid, TestAuthorInput, TestAuthoringMetrics, TestManifest, TestRun,
  Verdict, VerifierInput, VerifyMetrics, WaveBrief, WaveInput, WaveMetrics,
  WaveReport, WaveResult, WorkspaceInfo,
}

// --- small shared helpers ----------------------------------------------------

fn strings(values: List(String)) -> json.Json {
  json.array(values, json.string)
}

// --- enum tags ----------------------------------------------------------------

fn category_to_json(category: Category) -> json.Json {
  json.string(types.category_to_string(category))
}

fn category_decoder() -> decode.Decoder(Category) {
  use tag <- decode.then(decode.string)
  case types.category_from_string(tag) {
    option.Some(category) -> decode.success(category)
    option.None -> decode.failure(types.Correction, "Category")
  }
}

fn ruling_to_json(ruling: Ruling) -> json.Json {
  json.string(types.ruling_to_string(ruling))
}

fn ruling_decoder() -> decode.Decoder(Ruling) {
  use tag <- decode.then(decode.string)
  case types.ruling_from_string(tag) {
    option.Some(ruling) -> decode.success(ruling)
    option.None -> decode.failure(types.Fixed, "Ruling")
  }
}

fn disposition_to_json(disposition: Disposition) -> json.Json {
  json.string(types.disposition_to_string(disposition))
}

fn disposition_decoder() -> decode.Decoder(Disposition) {
  use tag <- decode.then(decode.string)
  case types.disposition_from_string(tag) {
    option.Some(disposition) -> decode.success(disposition)
    option.None -> decode.failure(types.Accepted, "Disposition")
  }
}

fn artifact_kind_to_json(kind: ArtifactKind) -> json.Json {
  json.string(types.artifact_kind_to_string(kind))
}

fn artifact_kind_decoder() -> decode.Decoder(ArtifactKind) {
  use tag <- decode.then(decode.string)
  case types.artifact_kind_from_string(tag) {
    option.Some(kind) -> decode.success(kind)
    option.None -> decode.failure(types.DispositionArtifact, "ArtifactKind")
  }
}

// --- ledger entries -------------------------------------------------------------

fn ledger_entry_to_json(entry: LedgerEntry) -> json.Json {
  json.object([
    #("id", json.string(entry.id)),
    #("title", json.string(entry.title)),
    #("file", json.string(entry.file)),
    #("line", json.int(entry.line)),
    #("category", category_to_json(entry.category)),
    #("severity", json.string(entry.severity)),
    #("detail", json.string(entry.detail)),
    #("failure_scenario", json.string(entry.failure_scenario)),
    #("recommendation", json.string(entry.recommendation)),
  ])
}

/// Decode a ledger entry from its agent-facing fields. TOLERANT of the extra
/// bookkeeping fields a full `ledger-entry.schema.json` entry carries
/// (`status`, `status_history`, join metadata, ...) — they are the applier's
/// domain and are simply not read.
fn ledger_entry_decoder() -> decode.Decoder(LedgerEntry) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use category <- decode.field("category", category_decoder())
  use severity <- decode.field("severity", decode.string)
  use detail <- decode.field("detail", decode.string)
  use failure_scenario <- decode.field("failure_scenario", decode.string)
  use recommendation <- decode.field("recommendation", decode.string)
  decode.success(LedgerEntry(
    id: id,
    title: title,
    file: file,
    line: line,
    category: category,
    severity: severity,
    detail: detail,
    failure_scenario: failure_scenario,
    recommendation: recommendation,
  ))
}

/// Encode the test-author's view of an entry. There is deliberately NO
/// `recommendation` pair here — the [`TestAuthorEntry`] type has no such
/// field, so this encoder cannot leak one.
fn test_author_entry_to_json(entry: TestAuthorEntry) -> json.Json {
  json.object([
    #("id", json.string(entry.id)),
    #("title", json.string(entry.title)),
    #("file", json.string(entry.file)),
    #("line", json.int(entry.line)),
    #("category", category_to_json(entry.category)),
    #("severity", json.string(entry.severity)),
    #("detail", json.string(entry.detail)),
    #("failure_scenario", json.string(entry.failure_scenario)),
  ])
}

fn test_author_entry_decoder() -> decode.Decoder(TestAuthorEntry) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use category <- decode.field("category", category_decoder())
  use severity <- decode.field("severity", decode.string)
  use detail <- decode.field("detail", decode.string)
  use failure_scenario <- decode.field("failure_scenario", decode.string)
  decode.success(types.TestAuthorEntry(
    id: id,
    title: title,
    file: file,
    line: line,
    category: category,
    severity: severity,
    detail: detail,
    failure_scenario: failure_scenario,
  ))
}

// --- brief -----------------------------------------------------------------------

fn brief_to_json(brief: Brief) -> json.Json {
  json.object([
    #("id", json.string(brief.id)),
    #("finding_ids", strings(brief.finding_ids)),
    #("root_cause", json.string(brief.root_cause)),
    #(
      "scope",
      json.object([
        #("files_expected", strings(brief.files_expected)),
        #("boundaries", strings(brief.boundaries)),
      ]),
    ),
    #("acceptance", strings(brief.acceptance)),
    #("wave", json.int(brief.wave)),
    #("deep_cluster", json.bool(brief.deep_cluster)),
  ])
}

fn brief_decoder() -> decode.Decoder(Brief) {
  use id <- decode.field("id", decode.string)
  use finding_ids <- decode.field("finding_ids", decode.list(decode.string))
  use root_cause <- decode.field("root_cause", decode.string)
  use files_expected <- decode.then(decode.at(
    ["scope", "files_expected"],
    decode.list(decode.string),
  ))
  use boundaries <- decode.then(decode.at(
    ["scope", "boundaries"],
    decode.list(decode.string),
  ))
  use acceptance <- decode.field("acceptance", decode.list(decode.string))
  use wave <- decode.field("wave", decode.int)
  use deep_cluster <- decode.field("deep_cluster", decode.bool)
  decode.success(Brief(
    id: id,
    finding_ids: finding_ids,
    root_cause: root_cause,
    files_expected: files_expected,
    boundaries: boundaries,
    acceptance: acceptance,
    wave: wave,
    deep_cluster: deep_cluster,
  ))
}

// --- run config ---------------------------------------------------------------------

fn config_to_json(config: RunConfig) -> json.Json {
  json.object([
    #("repo_root", json.string(config.repo_root)),
    #("ledger_path", json.string(config.ledger_path)),
    #("base_branch", json.string(config.base_branch)),
    #("max_fix_cycles", json.int(config.max_fix_cycles)),
  ])
}

fn config_decoder() -> decode.Decoder(RunConfig) {
  use repo_root <- decode.field("repo_root", decode.string)
  use ledger_path <- decode.field("ledger_path", decode.string)
  use base_branch <- decode.optional_field(
    "base_branch",
    types.default_base_branch(),
    decode.string,
  )
  use max_fix_cycles <- decode.optional_field(
    "max_fix_cycles",
    types.default_max_fix_cycles(),
    decode.int,
  )
  decode.success(RunConfig(
    repo_root: repo_root,
    ledger_path: ledger_path,
    base_branch: base_branch,
    max_fix_cycles: max_fix_cycles,
  ))
}

// --- child input: BriefInput ----------------------------------------------------------

/// The child `remediation_brief` workflow input codec.
pub fn brief_input_codec() -> Codec(BriefInput) {
  codec.json_codec(brief_input_to_json, brief_input_decoder())
}

fn brief_input_to_json(input: BriefInput) -> json.Json {
  json.object([
    #("brief", brief_to_json(input.brief)),
    #("entries", json.array(input.entries, ledger_entry_to_json)),
    #("config", config_to_json(input.config)),
  ])
}

fn brief_input_decoder() -> decode.Decoder(BriefInput) {
  use brief <- decode.field("brief", brief_decoder())
  use entries <- decode.field("entries", decode.list(ledger_entry_decoder()))
  use config <- decode.field("config", config_decoder())
  decode.success(BriefInput(brief: brief, entries: entries, config: config))
}

// --- agent input: test_author -----------------------------------------------------------

/// The `test_author` activity input codec. Encodes [`TestAuthorEntry`] values
/// exclusively — the recommendation-free projection — so the recommendation
/// field is stripped AT THE CODEC LAYER and can never reach the wire.
pub fn test_author_input_codec() -> Codec(TestAuthorInput) {
  codec.json_codec(test_author_input_to_json, test_author_input_decoder())
}

fn test_author_input_to_json(input: TestAuthorInput) -> json.Json {
  json.object([
    #("brief", brief_to_json(input.brief)),
    #("entries", json.array(input.entries, test_author_entry_to_json)),
  ])
}

fn test_author_input_decoder() -> decode.Decoder(TestAuthorInput) {
  use brief <- decode.field("brief", brief_decoder())
  use entries <- decode.field(
    "entries",
    decode.list(test_author_entry_decoder()),
  )
  decode.success(TestAuthorInput(brief: brief, entries: entries))
}

// --- agent input: developer ---------------------------------------------------------------

/// The `developer` activity input codec: full entries (recommendation
/// included), the manifest, gate-1 evidence, and the loop-back artifacts.
pub fn developer_input_codec() -> Codec(DeveloperInput) {
  codec.json_codec(developer_input_to_json, developer_input_decoder())
}

fn developer_input_to_json(input: DeveloperInput) -> json.Json {
  json.object([
    #("brief", brief_to_json(input.brief)),
    #("entries", json.array(input.entries, ledger_entry_to_json)),
    #("test_manifest", test_manifest_to_json(input.manifest)),
    #("gate1_results", json.array(input.gate1_results, test_run_to_json)),
    #("verdict", json.nullable(input.verdict, verdict_to_json)),
    #("gate2", json.nullable(input.gate2, gate2_outcome_to_json)),
  ])
}

fn developer_input_decoder() -> decode.Decoder(DeveloperInput) {
  use brief <- decode.field("brief", brief_decoder())
  use entries <- decode.field("entries", decode.list(ledger_entry_decoder()))
  use manifest <- decode.field("test_manifest", test_manifest_decoder())
  use gate1_results <- decode.field(
    "gate1_results",
    decode.list(test_run_decoder()),
  )
  use verdict <- decode.field("verdict", decode.optional(verdict_decoder()))
  use gate2 <- decode.field("gate2", decode.optional(gate2_outcome_decoder()))
  decode.success(DeveloperInput(
    brief: brief,
    entries: entries,
    manifest: manifest,
    gate1_results: gate1_results,
    verdict: verdict,
    gate2: gate2,
  ))
}

// --- agent input: verifier -------------------------------------------------------------------

/// The `verifier` activity input codec: findings + diff + fix report + test
/// manifest (DESIGN.md Stage 3 inputs).
pub fn verifier_input_codec() -> Codec(VerifierInput) {
  codec.json_codec(verifier_input_to_json, verifier_input_decoder())
}

fn verifier_input_to_json(input: VerifierInput) -> json.Json {
  json.object([
    #("brief", brief_to_json(input.brief)),
    #("entries", json.array(input.entries, ledger_entry_to_json)),
    #("test_manifest", test_manifest_to_json(input.manifest)),
    #("fix_report", fix_report_to_json(input.fix_report)),
    #("diff", json.string(input.diff)),
  ])
}

fn verifier_input_decoder() -> decode.Decoder(VerifierInput) {
  use brief <- decode.field("brief", brief_decoder())
  use entries <- decode.field("entries", decode.list(ledger_entry_decoder()))
  use manifest <- decode.field("test_manifest", test_manifest_decoder())
  use fix_report <- decode.field("fix_report", fix_report_decoder())
  use diff <- decode.field("diff", decode.string)
  decode.success(VerifierInput(
    brief: brief,
    entries: entries,
    manifest: manifest,
    fix_report: fix_report,
    diff: diff,
  ))
}

// --- agent output: TestManifest ----------------------------------------------------------------

/// The test-author output codec (`test-manifest.schema.json`).
pub fn test_manifest_codec() -> Codec(TestManifest) {
  codec.json_codec(test_manifest_to_json, test_manifest_decoder())
}

fn manifest_entry_to_json(entry: ManifestEntry) -> json.Json {
  json.object([
    #("finding_id", json.string(entry.finding_id)),
    #("test_names", strings(entry.test_names)),
    #("test_file", json.string(entry.test_file)),
    #(
      "expected_failure_signature",
      json.string(entry.expected_failure_signature),
    ),
    #("fail_evidence", json.string(entry.fail_evidence)),
    #("could_not_reproduce", json.bool(entry.could_not_reproduce)),
    #(
      "could_not_reproduce_reason",
      json.nullable(entry.could_not_reproduce_reason, json.string),
    ),
    #("manual_acceptance", json.nullable(entry.manual_acceptance, json.string)),
  ])
}

fn manifest_entry_decoder() -> decode.Decoder(ManifestEntry) {
  use finding_id <- decode.field("finding_id", decode.string)
  use test_names <- decode.field("test_names", decode.list(decode.string))
  use test_file <- decode.field("test_file", decode.string)
  use expected_failure_signature <- decode.field(
    "expected_failure_signature",
    decode.string,
  )
  use fail_evidence <- decode.field("fail_evidence", decode.string)
  use could_not_reproduce <- decode.field("could_not_reproduce", decode.bool)
  // The two nullable fields are OPTIONAL on decode (absent == null): the
  // schema requires them present-or-null on Norn's output, but a tolerant
  // read here keeps older recorded histories decodable.
  use could_not_reproduce_reason <- decode.optional_field(
    "could_not_reproduce_reason",
    option.None,
    decode.optional(decode.string),
  )
  use manual_acceptance <- decode.optional_field(
    "manual_acceptance",
    option.None,
    decode.optional(decode.string),
  )
  decode.success(ManifestEntry(
    finding_id: finding_id,
    test_names: test_names,
    test_file: test_file,
    expected_failure_signature: expected_failure_signature,
    fail_evidence: fail_evidence,
    could_not_reproduce: could_not_reproduce,
    could_not_reproduce_reason: could_not_reproduce_reason,
    manual_acceptance: manual_acceptance,
  ))
}

fn test_manifest_to_json(manifest: TestManifest) -> json.Json {
  json.object([
    #("brief_id", json.string(manifest.brief_id)),
    #("entries", json.array(manifest.entries, manifest_entry_to_json)),
  ])
}

fn test_manifest_decoder() -> decode.Decoder(TestManifest) {
  use brief_id <- decode.field("brief_id", decode.string)
  use entries <- decode.field("entries", decode.list(manifest_entry_decoder()))
  decode.success(TestManifest(brief_id: brief_id, entries: entries))
}

// --- agent output: FixReport ---------------------------------------------------------------------

/// The developer output codec (`fix-report.schema.json`).
pub fn fix_report_codec() -> Codec(FixReport) {
  codec.json_codec(fix_report_to_json, fix_report_decoder())
}

fn finding_fix_to_json(fix: FindingFix) -> json.Json {
  json.object([
    #("finding_id", json.string(fix.finding_id)),
    #("how", json.string(fix.how)),
  ])
}

fn finding_fix_decoder() -> decode.Decoder(FindingFix) {
  use finding_id <- decode.field("finding_id", decode.string)
  use how <- decode.field("how", decode.string)
  decode.success(FindingFix(finding_id: finding_id, how: how))
}

fn deviation_to_json(deviation: Deviation) -> json.Json {
  json.object([
    #("what", json.string(deviation.what)),
    #("why", json.string(deviation.why)),
    #("approved_by", json.string(deviation.approved_by)),
  ])
}

fn deviation_decoder() -> decode.Decoder(Deviation) {
  use what <- decode.field("what", decode.string)
  use why <- decode.field("why", decode.string)
  use approved_by <- decode.field("approved_by", decode.string)
  decode.success(Deviation(what: what, why: why, approved_by: approved_by))
}

fn finding_bounce_to_json(bounce: FindingBounce) -> json.Json {
  json.object([
    #("finding_id", json.string(bounce.finding_id)),
    #("reason", json.string(bounce.reason)),
  ])
}

fn finding_bounce_decoder() -> decode.Decoder(FindingBounce) {
  use finding_id <- decode.field("finding_id", decode.string)
  use reason <- decode.field("reason", decode.string)
  decode.success(FindingBounce(finding_id: finding_id, reason: reason))
}

fn class_instance_to_json(instance: ClassInstance) -> json.Json {
  json.object([
    #("file", json.string(instance.file)),
    #("line", json.int(instance.line)),
    #("fixed", json.bool(instance.fixed)),
    #("note", json.string(instance.note)),
  ])
}

fn class_instance_decoder() -> decode.Decoder(ClassInstance) {
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use fixed <- decode.field("fixed", decode.bool)
  use note <- decode.field("note", decode.string)
  decode.success(ClassInstance(file: file, line: line, fixed: fixed, note: note))
}

fn fix_report_to_json(report: FixReport) -> json.Json {
  json.object([
    #("brief_id", json.string(report.brief_id)),
    #("commits", strings(report.commits)),
    #(
      "findings_addressed",
      json.array(report.findings_addressed, finding_fix_to_json),
    ),
    #(
      "findings_bounced",
      json.array(report.findings_bounced, finding_bounce_to_json),
    ),
    #("deviations", json.array(report.deviations, deviation_to_json)),
    #("new_tests", strings(report.new_tests)),
    #(
      "class_instances_found",
      json.array(report.class_instances_found, class_instance_to_json),
    ),
  ])
}

fn fix_report_decoder() -> decode.Decoder(FixReport) {
  use brief_id <- decode.field("brief_id", decode.string)
  use commits <- decode.field("commits", decode.list(decode.string))
  use findings_addressed <- decode.field(
    "findings_addressed",
    decode.list(finding_fix_decoder()),
  )
  use findings_bounced <- decode.field(
    "findings_bounced",
    decode.list(finding_bounce_decoder()),
  )
  use deviations <- decode.field("deviations", decode.list(deviation_decoder()))
  use new_tests <- decode.field("new_tests", decode.list(decode.string))
  use class_instances_found <- decode.field(
    "class_instances_found",
    decode.list(class_instance_decoder()),
  )
  decode.success(FixReport(
    brief_id: brief_id,
    commits: commits,
    findings_addressed: findings_addressed,
    findings_bounced: findings_bounced,
    deviations: deviations,
    new_tests: new_tests,
    class_instances_found: class_instances_found,
  ))
}

// --- agent output: Verdict -------------------------------------------------------------------------

/// The verifier output codec (`verdict.schema.json`).
pub fn verdict_codec() -> Codec(Verdict) {
  codec.json_codec(verdict_to_json, verdict_decoder())
}

fn finding_ruling_to_json(ruling: FindingRuling) -> json.Json {
  json.object([
    #("finding_id", json.string(ruling.finding_id)),
    #("ruling", ruling_to_json(ruling.ruling)),
    #("evidence", json.string(ruling.evidence)),
  ])
}

fn finding_ruling_decoder() -> decode.Decoder(FindingRuling) {
  use finding_id <- decode.field("finding_id", decode.string)
  use ruling <- decode.field("ruling", ruling_decoder())
  use evidence <- decode.field("evidence", decode.string)
  decode.success(FindingRuling(
    finding_id: finding_id,
    ruling: ruling,
    evidence: evidence,
  ))
}

fn class_sibling_to_json(sibling: ClassSibling) -> json.Json {
  json.object([
    #("file", json.string(sibling.file)),
    #("line", json.int(sibling.line)),
    #("description", json.string(sibling.description)),
  ])
}

fn class_sibling_decoder() -> decode.Decoder(ClassSibling) {
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use description <- decode.field("description", decode.string)
  decode.success(ClassSibling(file: file, line: line, description: description))
}

fn regression_risk_to_json(risk: RegressionRisk) -> json.Json {
  json.object([
    #("file", json.string(risk.file)),
    #("concern", json.string(risk.concern)),
  ])
}

fn regression_risk_decoder() -> decode.Decoder(RegressionRisk) {
  use file <- decode.field("file", decode.string)
  use concern <- decode.field("concern", decode.string)
  decode.success(RegressionRisk(file: file, concern: concern))
}

fn overall_to_json(overall: Overall) -> json.Json {
  json.string(types.overall_to_string(overall))
}

fn overall_decoder() -> decode.Decoder(Overall) {
  use tag <- decode.then(decode.string)
  case types.overall_from_string(tag) {
    option.Some(overall) -> decode.success(overall)
    option.None -> decode.failure(types.Reject, "Overall")
  }
}

fn verdict_to_json(verdict: Verdict) -> json.Json {
  json.object([
    #("brief_id", json.string(verdict.brief_id)),
    #("per_finding", json.array(verdict.per_finding, finding_ruling_to_json)),
    #(
      "class_siblings_found",
      json.array(verdict.class_siblings_found, class_sibling_to_json),
    ),
    #(
      "regression_risks",
      json.array(verdict.regression_risks, regression_risk_to_json),
    ),
    #("standards_violations", strings(verdict.standards_violations)),
    #("overall", overall_to_json(verdict.overall)),
    #("reject_reason", json.nullable(verdict.reject_reason, json.string)),
  ])
}

fn verdict_decoder() -> decode.Decoder(Verdict) {
  use brief_id <- decode.field("brief_id", decode.string)
  use per_finding <- decode.field(
    "per_finding",
    decode.list(finding_ruling_decoder()),
  )
  use class_siblings_found <- decode.field(
    "class_siblings_found",
    decode.list(class_sibling_decoder()),
  )
  use regression_risks <- decode.field(
    "regression_risks",
    decode.list(regression_risk_decoder()),
  )
  use standards_violations <- decode.field(
    "standards_violations",
    decode.list(decode.string),
  )
  use overall <- decode.field("overall", overall_decoder())
  use reject_reason <- decode.field(
    "reject_reason",
    decode.optional(decode.string),
  )
  decode.success(Verdict(
    brief_id: brief_id,
    per_finding: per_finding,
    class_siblings_found: class_siblings_found,
    regression_risks: regression_risks,
    standards_violations: standards_violations,
    overall: overall,
    reject_reason: reject_reason,
  ))
}

// --- shell activity: provision -----------------------------------------------------------------------

pub fn provision_input_codec() -> Codec(ProvisionInput) {
  codec.json_codec(provision_input_to_json, provision_input_decoder())
}

fn provision_input_to_json(input: ProvisionInput) -> json.Json {
  json.object([
    #("repo_root", json.string(input.repo_root)),
    #("base_branch", json.string(input.base_branch)),
    #("branch", json.string(input.branch)),
    #("workspace_path", json.string(input.workspace_path)),
  ])
}

fn provision_input_decoder() -> decode.Decoder(ProvisionInput) {
  use repo_root <- decode.field("repo_root", decode.string)
  use base_branch <- decode.field("base_branch", decode.string)
  use branch <- decode.field("branch", decode.string)
  use workspace_path <- decode.field("workspace_path", decode.string)
  decode.success(ProvisionInput(
    repo_root: repo_root,
    base_branch: base_branch,
    branch: branch,
    workspace_path: workspace_path,
  ))
}

pub fn workspace_info_codec() -> Codec(WorkspaceInfo) {
  codec.json_codec(workspace_info_to_json, workspace_info_decoder())
}

fn workspace_info_to_json(info: WorkspaceInfo) -> json.Json {
  json.object([
    #("workspace_path", json.string(info.workspace_path)),
    #("branch", json.string(info.branch)),
    #("base_commit", json.string(info.base_commit)),
  ])
}

fn workspace_info_decoder() -> decode.Decoder(WorkspaceInfo) {
  use workspace_path <- decode.field("workspace_path", decode.string)
  use branch <- decode.field("branch", decode.string)
  use base_commit <- decode.field("base_commit", decode.string)
  decode.success(WorkspaceInfo(
    workspace_path: workspace_path,
    branch: branch,
    base_commit: base_commit,
  ))
}

// --- shell activity: gate1 ---------------------------------------------------------------------------

pub fn gate1_input_codec() -> Codec(Gate1Input) {
  codec.json_codec(gate1_input_to_json, gate1_input_decoder())
}

fn gate1_check_to_json(check: Gate1Check) -> json.Json {
  json.object([
    #("finding_id", json.string(check.finding_id)),
    #("test_names", strings(check.test_names)),
    #(
      "expected_failure_signature",
      json.string(check.expected_failure_signature),
    ),
  ])
}

fn gate1_check_decoder() -> decode.Decoder(Gate1Check) {
  use finding_id <- decode.field("finding_id", decode.string)
  use test_names <- decode.field("test_names", decode.list(decode.string))
  use expected_failure_signature <- decode.field(
    "expected_failure_signature",
    decode.string,
  )
  decode.success(Gate1Check(
    finding_id: finding_id,
    test_names: test_names,
    expected_failure_signature: expected_failure_signature,
  ))
}

fn acceptance_check_to_json(check: AcceptanceCheck) -> json.Json {
  json.object([
    #("finding_id", json.string(check.finding_id)),
    #("criterion", json.string(check.criterion)),
  ])
}

fn acceptance_check_decoder() -> decode.Decoder(AcceptanceCheck) {
  use finding_id <- decode.field("finding_id", decode.string)
  use criterion <- decode.field("criterion", decode.string)
  decode.success(AcceptanceCheck(finding_id: finding_id, criterion: criterion))
}

fn gate1_input_to_json(input: Gate1Input) -> json.Json {
  json.object([
    #("workspace_path", json.string(input.workspace_path)),
    #("base_commit", json.string(input.base_commit)),
    #("checks", json.array(input.checks, gate1_check_to_json)),
    #("acceptance", json.array(input.acceptance, acceptance_check_to_json)),
    #("test_files", strings(input.test_files)),
  ])
}

fn gate1_input_decoder() -> decode.Decoder(Gate1Input) {
  use workspace_path <- decode.field("workspace_path", decode.string)
  use base_commit <- decode.field("base_commit", decode.string)
  use checks <- decode.field("checks", decode.list(gate1_check_decoder()))
  use acceptance <- decode.field(
    "acceptance",
    decode.list(acceptance_check_decoder()),
  )
  use test_files <- decode.field("test_files", decode.list(decode.string))
  decode.success(Gate1Input(
    workspace_path: workspace_path,
    base_commit: base_commit,
    checks: checks,
    acceptance: acceptance,
    test_files: test_files,
  ))
}

fn test_run_to_json(run: TestRun) -> json.Json {
  json.object([
    #("finding_id", json.string(run.finding_id)),
    #("test_name", json.string(run.test_name)),
    #("failed", json.bool(run.failed)),
    #("signature_matched", json.bool(run.signature_matched)),
    #("evidence", json.string(run.evidence)),
  ])
}

fn test_run_decoder() -> decode.Decoder(TestRun) {
  use finding_id <- decode.field("finding_id", decode.string)
  use test_name <- decode.field("test_name", decode.string)
  use failed <- decode.field("failed", decode.bool)
  use signature_matched <- decode.field("signature_matched", decode.bool)
  use evidence <- decode.field("evidence", decode.string)
  decode.success(TestRun(
    finding_id: finding_id,
    test_name: test_name,
    failed: failed,
    signature_matched: signature_matched,
    evidence: evidence,
  ))
}

pub fn gate1_outcome_codec() -> Codec(Gate1Outcome) {
  codec.json_codec(gate1_outcome_to_json, gate1_outcome_decoder())
}

fn gate1_outcome_to_json(outcome: Gate1Outcome) -> json.Json {
  json.object([
    #("pass", json.bool(outcome.pass)),
    #("results", json.array(outcome.results, test_run_to_json)),
    #(
      "acceptance_checks",
      json.array(outcome.acceptance_checks, acceptance_check_to_json),
    ),
    #("scope_violations", strings(outcome.scope_violations)),
    #("authored_test_paths", strings(outcome.authored_test_paths)),
    #("tests_commit", json.string(outcome.tests_commit)),
    #("detail", json.string(outcome.detail)),
  ])
}

fn gate1_outcome_decoder() -> decode.Decoder(Gate1Outcome) {
  use pass <- decode.field("pass", decode.bool)
  use results <- decode.field("results", decode.list(test_run_decoder()))
  use acceptance_checks <- decode.field(
    "acceptance_checks",
    decode.list(acceptance_check_decoder()),
  )
  use scope_violations <- decode.field(
    "scope_violations",
    decode.list(decode.string),
  )
  use authored_test_paths <- decode.field(
    "authored_test_paths",
    decode.list(decode.string),
  )
  use tests_commit <- decode.field("tests_commit", decode.string)
  use detail <- decode.field("detail", decode.string)
  decode.success(Gate1Outcome(
    pass: pass,
    results: results,
    acceptance_checks: acceptance_checks,
    scope_violations: scope_violations,
    authored_test_paths: authored_test_paths,
    tests_commit: tests_commit,
    detail: detail,
  ))
}

// --- shell activity: gate2 -----------------------------------------------------------------------------

pub fn gate2_input_codec() -> Codec(Gate2Input) {
  codec.json_codec(gate2_input_to_json, gate2_input_decoder())
}

fn gate2_input_to_json(input: Gate2Input) -> json.Json {
  json.object([
    #("workspace_path", json.string(input.workspace_path)),
    #("tests_commit", json.string(input.tests_commit)),
    #("authored_test_paths", strings(input.authored_test_paths)),
  ])
}

fn gate2_input_decoder() -> decode.Decoder(Gate2Input) {
  use workspace_path <- decode.field("workspace_path", decode.string)
  use tests_commit <- decode.field("tests_commit", decode.string)
  use authored_test_paths <- decode.field(
    "authored_test_paths",
    decode.list(decode.string),
  )
  decode.success(Gate2Input(
    workspace_path: workspace_path,
    tests_commit: tests_commit,
    authored_test_paths: authored_test_paths,
  ))
}

pub fn gate2_outcome_codec() -> Codec(Gate2Outcome) {
  codec.json_codec(gate2_outcome_to_json, gate2_outcome_decoder())
}

fn gate2_outcome_to_json(outcome: Gate2Outcome) -> json.Json {
  json.object([
    #("pass", json.bool(outcome.pass)),
    #("test_diff_clean", json.bool(outcome.test_diff_clean)),
    #("clippy_pass", json.bool(outcome.clippy_pass)),
    #("suite_pass", json.bool(outcome.suite_pass)),
    #("diagnostics", json.string(outcome.diagnostics)),
    #("diff", json.string(outcome.diff)),
  ])
}

fn gate2_outcome_decoder() -> decode.Decoder(Gate2Outcome) {
  use pass <- decode.field("pass", decode.bool)
  use test_diff_clean <- decode.field("test_diff_clean", decode.bool)
  use clippy_pass <- decode.field("clippy_pass", decode.bool)
  use suite_pass <- decode.field("suite_pass", decode.bool)
  use diagnostics <- decode.field("diagnostics", decode.string)
  use diff <- decode.field("diff", decode.string)
  decode.success(Gate2Outcome(
    pass: pass,
    test_diff_clean: test_diff_clean,
    clippy_pass: clippy_pass,
    suite_pass: suite_pass,
    diagnostics: diagnostics,
    diff: diff,
  ))
}

// --- shell activity: ledger_update ----------------------------------------------------------------------

pub fn ledger_update_input_codec() -> Codec(LedgerUpdateInput) {
  codec.json_codec(ledger_update_input_to_json, ledger_update_input_decoder())
}

fn ledger_update_input_to_json(input: LedgerUpdateInput) -> json.Json {
  json.object([
    #("repo_root", json.string(input.repo_root)),
    #("ledger_path", json.string(input.ledger_path)),
    #("kind", artifact_kind_to_json(input.kind)),
    #("artifact_json", json.string(input.artifact_json)),
  ])
}

fn ledger_update_input_decoder() -> decode.Decoder(LedgerUpdateInput) {
  use repo_root <- decode.field("repo_root", decode.string)
  use ledger_path <- decode.field("ledger_path", decode.string)
  use kind <- decode.field("kind", artifact_kind_decoder())
  use artifact_json <- decode.field("artifact_json", decode.string)
  decode.success(LedgerUpdateInput(
    repo_root: repo_root,
    ledger_path: ledger_path,
    kind: kind,
    artifact_json: artifact_json,
  ))
}

pub fn ledger_update_outcome_codec() -> Codec(LedgerUpdateOutcome) {
  codec.json_codec(
    ledger_update_outcome_to_json,
    ledger_update_outcome_decoder(),
  )
}

fn ledger_update_outcome_to_json(outcome: LedgerUpdateOutcome) -> json.Json {
  json.object([
    #("applied", json.bool(outcome.applied)),
    #("detail", json.string(outcome.detail)),
  ])
}

fn ledger_update_outcome_decoder() -> decode.Decoder(LedgerUpdateOutcome) {
  use applied <- decode.field("applied", decode.bool)
  use detail <- decode.field("detail", decode.string)
  decode.success(LedgerUpdateOutcome(applied: applied, detail: detail))
}

// --- shell activity: cleanup ------------------------------------------------------------------------------

pub fn cleanup_input_codec() -> Codec(CleanupInput) {
  codec.json_codec(cleanup_input_to_json, cleanup_input_decoder())
}

fn cleanup_input_to_json(input: CleanupInput) -> json.Json {
  json.object([
    #("repo_root", json.string(input.repo_root)),
    #("workspace_path", json.string(input.workspace_path)),
  ])
}

fn cleanup_input_decoder() -> decode.Decoder(CleanupInput) {
  use repo_root <- decode.field("repo_root", decode.string)
  use workspace_path <- decode.field("workspace_path", decode.string)
  decode.success(CleanupInput(
    repo_root: repo_root,
    workspace_path: workspace_path,
  ))
}

pub fn cleanup_outcome_codec() -> Codec(CleanupOutcome) {
  codec.json_codec(cleanup_outcome_to_json, cleanup_outcome_decoder())
}

fn cleanup_outcome_to_json(outcome: CleanupOutcome) -> json.Json {
  json.object([
    #("removed", json.bool(outcome.removed)),
    #("detail", json.string(outcome.detail)),
  ])
}

fn cleanup_outcome_decoder() -> decode.Decoder(CleanupOutcome) {
  use removed <- decode.field("removed", decode.bool)
  use detail <- decode.field("detail", decode.string)
  decode.success(CleanupOutcome(removed: removed, detail: detail))
}

// --- the disposition artifact ---------------------------------------------------------------------------------

/// Render the terminal-disposition artifact the `ledger_update` activity
/// applies with `--kind disposition`. The shape is part of the applier CLI
/// contract recorded in this example's README.
pub fn disposition_artifact_json(
  brief_id brief_id: String,
  disposition disposition: Disposition,
  fix_cycles fix_cycles: Int,
  test_edit_attempts test_edit_attempts: Int,
  could_not_reproduce could_not_reproduce: List(String),
  detail detail: String,
) -> String {
  json.object([
    #("brief_id", json.string(brief_id)),
    #("disposition", disposition_to_json(disposition)),
    #("fix_cycles", json.int(fix_cycles)),
    #("test_edit_attempts", json.int(test_edit_attempts)),
    #("could_not_reproduce", strings(could_not_reproduce)),
    #("detail", json.string(detail)),
  ])
  |> json.to_string
}

/// Render a test manifest as the artifact JSON for `--kind test_manifest`.
pub fn test_manifest_artifact_json(manifest: TestManifest) -> String {
  test_manifest_to_json(manifest) |> json.to_string
}

/// Render a fix report as the artifact JSON for `--kind fix_report`.
pub fn fix_report_artifact_json(report: FixReport) -> String {
  fix_report_to_json(report) |> json.to_string
}

/// Render a verdict as the artifact JSON for `--kind verdict`.
pub fn verdict_artifact_json(verdict: Verdict) -> String {
  verdict_to_json(verdict) |> json.to_string
}

// --- child result ------------------------------------------------------------------------------------------------

fn ledger_application_to_json(application: LedgerApplication) -> json.Json {
  json.object([
    #("kind", json.string(application.kind)),
    #("applied", json.bool(application.applied)),
    #("detail", json.string(application.detail)),
  ])
}

fn ledger_application_decoder() -> decode.Decoder(LedgerApplication) {
  use kind <- decode.field("kind", decode.string)
  use applied <- decode.field("applied", decode.bool)
  use detail <- decode.field("detail", decode.string)
  decode.success(LedgerApplication(kind: kind, applied: applied, detail: detail))
}

pub fn brief_result_codec() -> Codec(BriefResult) {
  codec.json_codec(brief_result_to_json, brief_result_decoder())
}

fn brief_result_to_json(result: BriefResult) -> json.Json {
  json.object([
    #("brief_id", json.string(result.brief_id)),
    #("disposition", disposition_to_json(result.disposition)),
    #("fix_cycles", json.int(result.fix_cycles)),
    #("first_pass_accepted", json.bool(result.first_pass_accepted)),
    #("could_not_reproduce", strings(result.could_not_reproduce)),
    #("test_edit_attempts", json.int(result.test_edit_attempts)),
    #("verdict_mismatches", strings(result.verdict_mismatches)),
    #("branch", json.string(result.branch)),
    #("test_manifest", test_manifest_to_json(result.manifest)),
    #("fix_report", json.nullable(result.fix_report, fix_report_to_json)),
    #("verdict", json.nullable(result.verdict, verdict_to_json)),
    #("ledger", json.array(result.ledger, ledger_application_to_json)),
    #("workspace_removed", json.bool(result.workspace_removed)),
    #("summary", json.string(result.summary)),
  ])
}

fn brief_result_decoder() -> decode.Decoder(BriefResult) {
  use brief_id <- decode.field("brief_id", decode.string)
  use disposition <- decode.field("disposition", disposition_decoder())
  use fix_cycles <- decode.field("fix_cycles", decode.int)
  use first_pass_accepted <- decode.field("first_pass_accepted", decode.bool)
  use could_not_reproduce <- decode.field(
    "could_not_reproduce",
    decode.list(decode.string),
  )
  use test_edit_attempts <- decode.field("test_edit_attempts", decode.int)
  use verdict_mismatches <- decode.field(
    "verdict_mismatches",
    decode.list(decode.string),
  )
  use branch <- decode.field("branch", decode.string)
  use manifest <- decode.field("test_manifest", test_manifest_decoder())
  use fix_report <- decode.field(
    "fix_report",
    decode.optional(fix_report_decoder()),
  )
  use verdict <- decode.field("verdict", decode.optional(verdict_decoder()))
  use ledger <- decode.field(
    "ledger",
    decode.list(ledger_application_decoder()),
  )
  use workspace_removed <- decode.field("workspace_removed", decode.bool)
  use summary <- decode.field("summary", decode.string)
  decode.success(BriefResult(
    brief_id: brief_id,
    disposition: disposition,
    fix_cycles: fix_cycles,
    first_pass_accepted: first_pass_accepted,
    could_not_reproduce: could_not_reproduce,
    test_edit_attempts: test_edit_attempts,
    verdict_mismatches: verdict_mismatches,
    branch: branch,
    manifest: manifest,
    fix_report: fix_report,
    verdict: verdict,
    ledger: ledger,
    workspace_removed: workspace_removed,
    summary: summary,
  ))
}

// --- parent input: WaveInput ------------------------------------------------------------------------------------

pub fn wave_input_codec() -> Codec(WaveInput) {
  codec.json_codec(wave_input_to_json, wave_input_decoder())
}

fn wave_brief_to_json(wave_brief: WaveBrief) -> json.Json {
  json.object([
    #("brief", brief_to_json(wave_brief.brief)),
    #("entries", json.array(wave_brief.entries, ledger_entry_to_json)),
  ])
}

fn wave_brief_decoder() -> decode.Decoder(WaveBrief) {
  use brief <- decode.field("brief", brief_decoder())
  use entries <- decode.field("entries", decode.list(ledger_entry_decoder()))
  decode.success(WaveBrief(brief: brief, entries: entries))
}

fn wave_input_to_json(input: WaveInput) -> json.Json {
  json.object([
    #("briefs", json.array(input.briefs, wave_brief_to_json)),
    #("strata", json.array(input.strata, strings)),
    #("config", config_to_json(input.config)),
  ])
}

fn wave_input_decoder() -> decode.Decoder(WaveInput) {
  use briefs <- decode.field("briefs", decode.list(wave_brief_decoder()))
  use strata <- decode.field("strata", decode.list(decode.list(decode.string)))
  use config <- decode.field("config", config_decoder())
  decode.success(WaveInput(briefs: briefs, strata: strata, config: config))
}

// --- wave report -----------------------------------------------------------------------------------------------

fn test_authoring_metrics_to_json(metrics: TestAuthoringMetrics) -> json.Json {
  json.object([
    #(
      "valid_fail_first_rate",
      json.nullable(metrics.valid_fail_first_rate, json.float),
    ),
    #(
      "wrong_reason_fail_rate",
      json.nullable(metrics.wrong_reason_fail_rate, json.float),
    ),
    #(
      "could_not_reproduce_rate",
      json.nullable(metrics.could_not_reproduce_rate, json.float),
    ),
  ])
}

fn test_authoring_metrics_decoder() -> decode.Decoder(TestAuthoringMetrics) {
  use valid_fail_first_rate <- decode.field(
    "valid_fail_first_rate",
    decode.optional(decode.float),
  )
  use wrong_reason_fail_rate <- decode.field(
    "wrong_reason_fail_rate",
    decode.optional(decode.float),
  )
  use could_not_reproduce_rate <- decode.field(
    "could_not_reproduce_rate",
    decode.optional(decode.float),
  )
  decode.success(TestAuthoringMetrics(
    valid_fail_first_rate: valid_fail_first_rate,
    wrong_reason_fail_rate: wrong_reason_fail_rate,
    could_not_reproduce_rate: could_not_reproduce_rate,
  ))
}

fn fix_metrics_to_json(metrics: FixMetrics) -> json.Json {
  json.object([
    #(
      "first_pass_acceptance_rate",
      json.nullable(metrics.first_pass_acceptance_rate, json.float),
    ),
    #(
      "fix_cycles_per_brief",
      json.nullable(metrics.fix_cycles_per_brief, json.float),
    ),
    #("deviation_count", json.nullable(metrics.deviation_count, json.int)),
    #("test_edit_attempts", json.nullable(metrics.test_edit_attempts, json.int)),
  ])
}

fn fix_metrics_decoder() -> decode.Decoder(FixMetrics) {
  use first_pass_acceptance_rate <- decode.field(
    "first_pass_acceptance_rate",
    decode.optional(decode.float),
  )
  use fix_cycles_per_brief <- decode.field(
    "fix_cycles_per_brief",
    decode.optional(decode.float),
  )
  use deviation_count <- decode.field(
    "deviation_count",
    decode.optional(decode.int),
  )
  use test_edit_attempts <- decode.field(
    "test_edit_attempts",
    decode.optional(decode.int),
  )
  decode.success(FixMetrics(
    first_pass_acceptance_rate: first_pass_acceptance_rate,
    fix_cycles_per_brief: fix_cycles_per_brief,
    deviation_count: deviation_count,
    test_edit_attempts: test_edit_attempts,
  ))
}

fn verify_metrics_to_json(metrics: VerifyMetrics) -> json.Json {
  json.object([
    #(
      "class_siblings_per_brief",
      json.nullable(metrics.class_siblings_per_brief, json.float),
    ),
    #(
      "verdicts_overturned",
      json.nullable(metrics.verdicts_overturned, json.int),
    ),
  ])
}

fn verify_metrics_decoder() -> decode.Decoder(VerifyMetrics) {
  use class_siblings_per_brief <- decode.field(
    "class_siblings_per_brief",
    decode.optional(decode.float),
  )
  use verdicts_overturned <- decode.field(
    "verdicts_overturned",
    decode.optional(decode.int),
  )
  decode.success(VerifyMetrics(
    class_siblings_per_brief: class_siblings_per_brief,
    verdicts_overturned: verdicts_overturned,
  ))
}

fn re_audit_metrics_to_json(metrics: ReAuditMetrics) -> json.Json {
  json.object([
    #(
      "class_recurrence_rate",
      json.nullable(metrics.class_recurrence_rate, json.float),
    ),
    #("new_finding_inflow", json.nullable(metrics.new_finding_inflow, json.int)),
  ])
}

fn re_audit_metrics_decoder() -> decode.Decoder(ReAuditMetrics) {
  use class_recurrence_rate <- decode.field(
    "class_recurrence_rate",
    decode.optional(decode.float),
  )
  use new_finding_inflow <- decode.field(
    "new_finding_inflow",
    decode.optional(decode.int),
  )
  decode.success(ReAuditMetrics(
    class_recurrence_rate: class_recurrence_rate,
    new_finding_inflow: new_finding_inflow,
  ))
}

fn flow_metrics_to_json(metrics: FlowMetrics) -> json.Json {
  json.object([
    #("lead_time_days", json.nullable(metrics.lead_time_days, json.float)),
    #(
      "terminal_state_ratio",
      json.nullable(metrics.terminal_state_ratio, json.float),
    ),
    // Ledger-derived finder calibration is the ledger-keeper's to fill; an
    // empty array is the honest "none computed here".
    #("refuted_rate_by_finder", json.array([], json.string)),
  ])
}

fn flow_metrics_decoder() -> decode.Decoder(FlowMetrics) {
  use lead_time_days <- decode.field(
    "lead_time_days",
    decode.optional(decode.float),
  )
  use terminal_state_ratio <- decode.field(
    "terminal_state_ratio",
    decode.optional(decode.float),
  )
  decode.success(FlowMetrics(
    lead_time_days: lead_time_days,
    terminal_state_ratio: terminal_state_ratio,
  ))
}

fn wave_metrics_to_json(metrics: WaveMetrics) -> json.Json {
  json.object([
    #("test_authoring", test_authoring_metrics_to_json(metrics.test_authoring)),
    #("fix", fix_metrics_to_json(metrics.fix)),
    #("verify", verify_metrics_to_json(metrics.verify)),
    #("re_audit", re_audit_metrics_to_json(metrics.re_audit)),
    #("flow", flow_metrics_to_json(metrics.flow)),
  ])
}

fn wave_metrics_decoder() -> decode.Decoder(WaveMetrics) {
  use test_authoring <- decode.field(
    "test_authoring",
    test_authoring_metrics_decoder(),
  )
  use fix <- decode.field("fix", fix_metrics_decoder())
  use verify <- decode.field("verify", verify_metrics_decoder())
  use re_audit <- decode.field("re_audit", re_audit_metrics_decoder())
  use flow <- decode.field("flow", flow_metrics_decoder())
  decode.success(WaveMetrics(
    test_authoring: test_authoring,
    fix: fix,
    verify: verify,
    re_audit: re_audit,
    flow: flow,
  ))
}

fn wave_report_to_json(report: WaveReport) -> json.Json {
  json.object([
    #("wave", json.int(report.wave)),
    #(
      "ledger_delta",
      json.object([
        #("new_entries", strings(report.new_entries)),
        // Transitions are applied (and therefore reported) by the applier, not
        // echoed back through this skeleton.
        #("transitions", json.array([], json.string)),
      ]),
    ),
    #("metrics", wave_metrics_to_json(report.metrics)),
    #("deferred_queue", strings(report.deferred_queue)),
    #("refuted_queue", strings(report.refuted_queue)),
  ])
}

fn wave_report_decoder() -> decode.Decoder(WaveReport) {
  use wave <- decode.field("wave", decode.int)
  use new_entries <- decode.then(decode.at(
    ["ledger_delta", "new_entries"],
    decode.list(decode.string),
  ))
  use metrics <- decode.field("metrics", wave_metrics_decoder())
  use deferred_queue <- decode.field(
    "deferred_queue",
    decode.list(decode.string),
  )
  use refuted_queue <- decode.field("refuted_queue", decode.list(decode.string))
  decode.success(WaveReport(
    wave: wave,
    new_entries: new_entries,
    metrics: metrics,
    deferred_queue: deferred_queue,
    refuted_queue: refuted_queue,
  ))
}

// --- parent result ------------------------------------------------------------------------------------------------

pub fn wave_result_codec() -> Codec(WaveResult) {
  codec.json_codec(wave_result_to_json, wave_result_decoder())
}

fn wave_result_to_json(result: WaveResult) -> json.Json {
  json.object([
    #("wave", json.int(result.wave)),
    #("briefs", json.array(result.briefs, brief_result_to_json)),
    #("report", wave_report_to_json(result.report)),
    #("summary", json.string(result.summary)),
  ])
}

fn wave_result_decoder() -> decode.Decoder(WaveResult) {
  use wave <- decode.field("wave", decode.int)
  use briefs <- decode.field("briefs", decode.list(brief_result_decoder()))
  use report <- decode.field("report", wave_report_decoder())
  use summary <- decode.field("summary", decode.string)
  decode.success(WaveResult(
    wave: wave,
    briefs: briefs,
    report: report,
    summary: summary,
  ))
}

// --- error codec -----------------------------------------------------------------------------------------------------

pub fn remediation_error_codec() -> Codec(RemediationError) {
  codec.json_codec(remediation_error_to_json, remediation_error_decoder())
}

fn remediation_error_to_json(error: RemediationError) -> json.Json {
  case error {
    StageFailed(stage: stage, message: message) ->
      json.object([
        #("kind", json.string("stage_failed")),
        #("stage", json.string(stage)),
        #("message", json.string(message)),
      ])
    StrataInvalid(reason: reason) ->
      json.object([
        #("kind", json.string("strata_invalid")),
        #("reason", json.string(reason)),
      ])
    ChildFailed(reason: reason) ->
      json.object([
        #("kind", json.string("child_failed")),
        #("reason", json.string(reason)),
      ])
    DecodeInputFailed(message: message) ->
      json.object([
        #("kind", json.string("decode_input_failed")),
        #("message", json.string(message)),
      ])
  }
}

fn remediation_error_decoder() -> decode.Decoder(RemediationError) {
  use kind <- decode.field("kind", decode.string)
  case kind {
    "strata_invalid" -> {
      use reason <- decode.field("reason", decode.string)
      decode.success(StrataInvalid(reason: reason))
    }
    "child_failed" -> {
      use reason <- decode.field("reason", decode.string)
      decode.success(ChildFailed(reason: reason))
    }
    "decode_input_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(DecodeInputFailed(message: message))
    }
    _ -> {
      use stage <- decode.optional_field("stage", "unknown", decode.string)
      use message <- decode.field("message", decode.string)
      decode.success(StageFailed(stage: stage, message: message))
    }
  }
}

// --- convenience -----------------------------------------------------------------------------------------------------

/// Build a wave-report metrics block with every field `None` — the base the
/// report builder fills computable fields into.
pub fn empty_metrics() -> WaveMetrics {
  WaveMetrics(
    test_authoring: TestAuthoringMetrics(
      valid_fail_first_rate: option.None,
      wrong_reason_fail_rate: option.None,
      could_not_reproduce_rate: option.None,
    ),
    fix: FixMetrics(
      first_pass_acceptance_rate: option.None,
      fix_cycles_per_brief: option.None,
      deviation_count: option.None,
      test_edit_attempts: option.None,
    ),
    verify: VerifyMetrics(
      class_siblings_per_brief: option.None,
      verdicts_overturned: option.None,
    ),
    re_audit: ReAuditMetrics(
      class_recurrence_rate: option.None,
      new_finding_inflow: option.None,
    ),
    flow: FlowMetrics(
      lead_time_days: option.None,
      terminal_state_ratio: option.None,
    ),
  )
}

/// Whether an optional value is present — a tiny readability helper for
/// result summaries.
pub fn is_some(value: Option(a)) -> Bool {
  case value {
    option.Some(_) -> True
    option.None -> False
  }
}
