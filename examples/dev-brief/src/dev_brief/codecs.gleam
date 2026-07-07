//// The JSON codecs binding every dev-brief type to its wire shape.
////
//// Three kinds of contract live here:
////
//// 1. The workflow input decoders ([`brief_input_codec`],
////    [`lens_input_codec`]) — tolerant of omitted overridable config values
////    (`base_branch`, `max_fix_cycles`, `lenses` default at the decode
////    layer or in the workflow; see `dev_brief/types` defaults).
//// 2. The two AGENT output codecs ([`dev_report_codec`],
////    [`lens_verdict_codec`]) — the Gleam mirror of the schemas the driven
////    harnesses enforce via `--output-schema`
////    (`schemas/dev-report.schema.json`, `schemas/lens-verdict.schema.json`).
//// 3. The SHELL activity wire codecs plus the pipeline result — each
////    byte-compatible with the Rust worker's serde types
////    (`worker/src/types.rs`).

import aion/codec.{type Codec}
import dev_brief/types.{
  type AcceptanceClaim, type Brief, type BriefInput, type BriefResult,
  type CleanupInput, type CleanupOutcome, type DevBriefError, type DevReport,
  type DeveloperInput, type Deviation, type Disposition, type GateCommand,
  type GateCommandRun, type GateInput, type GateOutcome, type Lens,
  type LensInput, type LensVerdict, type Overall, type ProvisionInput,
  type ReviewFinding, type RunConfig, type Severity, type WorkspaceInfo,
  AcceptanceClaim, Brief, BriefInput, BriefResult, ChildFailed, CleanupInput,
  CleanupOutcome, DecodeInputFailed, DevReport, DeveloperInput, Deviation,
  GateCommand, GateCommandRun, GateInput, GateOutcome, Lens, LensInput,
  LensVerdict, ProvisionInput, ReviewFinding, RunConfig, StageFailed,
  WorkspaceInfo,
}
import gleam/dynamic/decode
import gleam/json
import gleam/option

// --- small shared helpers ----------------------------------------------------

fn strings(values: List(String)) -> json.Json {
  json.array(values, json.string)
}

// --- enum tags ----------------------------------------------------------------

fn severity_to_json(severity: Severity) -> json.Json {
  json.string(types.severity_to_string(severity))
}

fn severity_decoder() -> decode.Decoder(Severity) {
  use tag <- decode.then(decode.string)
  case types.severity_from_string(tag) {
    option.Some(severity) -> decode.success(severity)
    option.None -> decode.failure(types.Blocking, "Severity")
  }
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

// --- the brief -------------------------------------------------------------------

fn brief_to_json(brief: Brief) -> json.Json {
  json.object([
    #("id", json.string(brief.id)),
    #("title", json.string(brief.title)),
    #("objective", json.string(brief.objective)),
    #("context", json.string(brief.context)),
    #("pointers", strings(brief.pointers)),
    #("scope_in", strings(brief.scope_in)),
    #("scope_out", strings(brief.scope_out)),
    #("acceptance", strings(brief.acceptance)),
    #("notes", json.string(brief.notes)),
  ])
}

fn brief_decoder() -> decode.Decoder(Brief) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use objective <- decode.field("objective", decode.string)
  use context <- decode.optional_field("context", "", decode.string)
  use pointers <- decode.optional_field(
    "pointers",
    [],
    decode.list(decode.string),
  )
  use scope_in <- decode.optional_field(
    "scope_in",
    [],
    decode.list(decode.string),
  )
  use scope_out <- decode.optional_field(
    "scope_out",
    [],
    decode.list(decode.string),
  )
  use acceptance <- decode.field("acceptance", decode.list(decode.string))
  use notes <- decode.optional_field("notes", "", decode.string)
  decode.success(Brief(
    id: id,
    title: title,
    objective: objective,
    context: context,
    pointers: pointers,
    scope_in: scope_in,
    scope_out: scope_out,
    acceptance: acceptance,
    notes: notes,
  ))
}

// --- run configuration ------------------------------------------------------------

fn gate_command_to_json(command: GateCommand) -> json.Json {
  json.object([
    #("name", json.string(command.name)),
    #("argv", strings(command.argv)),
  ])
}

fn gate_command_decoder() -> decode.Decoder(GateCommand) {
  use name <- decode.field("name", decode.string)
  use argv <- decode.field("argv", decode.list(decode.string))
  decode.success(GateCommand(name: name, argv: argv))
}

fn lens_to_json(lens: Lens) -> json.Json {
  json.object([
    #("name", json.string(lens.name)),
    #("charter", json.string(lens.charter)),
  ])
}

fn lens_decoder() -> decode.Decoder(Lens) {
  use name <- decode.field("name", decode.string)
  use charter <- decode.field("charter", decode.string)
  decode.success(Lens(name: name, charter: charter))
}

fn run_config_to_json(config: RunConfig) -> json.Json {
  json.object([
    #("repo_root", json.string(config.repo_root)),
    #("base_branch", json.string(config.base_branch)),
    #("gates", json.array(config.gates, gate_command_to_json)),
    #("max_fix_cycles", json.int(config.max_fix_cycles)),
    #("lenses", json.array(config.lenses, lens_to_json)),
  ])
}

fn run_config_decoder() -> decode.Decoder(RunConfig) {
  use repo_root <- decode.field("repo_root", decode.string)
  use base_branch <- decode.optional_field(
    "base_branch",
    types.default_base_branch(),
    decode.string,
  )
  use gates <- decode.optional_field(
    "gates",
    [],
    decode.list(gate_command_decoder()),
  )
  use max_fix_cycles <- decode.optional_field(
    "max_fix_cycles",
    types.default_max_fix_cycles(),
    decode.int,
  )
  // An OMITTED lenses key resolves to the default adversarial set in the
  // workflow (`dev_brief.resolve_lenses`) — the empty list is kept distinct
  // here so an explicit `"lenses": []` is still visible to that resolution.
  use lenses <- decode.optional_field("lenses", [], decode.list(lens_decoder()))
  decode.success(RunConfig(
    repo_root: repo_root,
    base_branch: base_branch,
    gates: gates,
    max_fix_cycles: max_fix_cycles,
    lenses: lenses,
  ))
}

/// The `dev_brief` workflow input codec.
pub fn brief_input_codec() -> Codec(BriefInput) {
  codec.json_codec(brief_input_to_json, brief_input_decoder())
}

fn brief_input_to_json(input: BriefInput) -> json.Json {
  json.object([
    #("brief", brief_to_json(input.brief)),
    #("config", run_config_to_json(input.config)),
  ])
}

fn brief_input_decoder() -> decode.Decoder(BriefInput) {
  use brief <- decode.field("brief", brief_decoder())
  use config <- decode.field("config", run_config_decoder())
  decode.success(BriefInput(brief: brief, config: config))
}

// --- developer report (agent output) ------------------------------------------------

fn acceptance_claim_to_json(claim: AcceptanceClaim) -> json.Json {
  json.object([
    #("criterion", json.string(claim.criterion)),
    #("how", json.string(claim.how)),
  ])
}

fn acceptance_claim_decoder() -> decode.Decoder(AcceptanceClaim) {
  use criterion <- decode.field("criterion", decode.string)
  use how <- decode.field("how", decode.string)
  decode.success(AcceptanceClaim(criterion: criterion, how: how))
}

fn deviation_to_json(deviation: Deviation) -> json.Json {
  json.object([
    #("what", json.string(deviation.what)),
    #("why", json.string(deviation.why)),
  ])
}

fn deviation_decoder() -> decode.Decoder(Deviation) {
  use what <- decode.field("what", decode.string)
  use why <- decode.field("why", decode.string)
  decode.success(Deviation(what: what, why: why))
}

fn dev_report_to_json(report: DevReport) -> json.Json {
  json.object([
    #("brief_id", json.string(report.brief_id)),
    #("summary", json.string(report.summary)),
    #("commits", strings(report.commits)),
    #(
      "acceptance_claims",
      json.array(report.acceptance_claims, acceptance_claim_to_json),
    ),
    #("deviations", json.array(report.deviations, deviation_to_json)),
  ])
}

fn dev_report_decoder() -> decode.Decoder(DevReport) {
  use brief_id <- decode.field("brief_id", decode.string)
  use summary <- decode.field("summary", decode.string)
  use commits <- decode.optional_field(
    "commits",
    [],
    decode.list(decode.string),
  )
  use acceptance_claims <- decode.field(
    "acceptance_claims",
    decode.list(acceptance_claim_decoder()),
  )
  use deviations <- decode.optional_field(
    "deviations",
    [],
    decode.list(deviation_decoder()),
  )
  decode.success(DevReport(
    brief_id: brief_id,
    summary: summary,
    commits: commits,
    acceptance_claims: acceptance_claims,
    deviations: deviations,
  ))
}

/// The developer agent's output codec (`dev-report.schema.json` mirror).
pub fn dev_report_codec() -> Codec(DevReport) {
  codec.json_codec(dev_report_to_json, dev_report_decoder())
}

/// The developer agent's input codec.
pub fn developer_input_codec() -> Codec(DeveloperInput) {
  codec.json_codec(developer_input_to_json, developer_input_decoder())
}

fn developer_input_to_json(input: DeveloperInput) -> json.Json {
  json.object([
    #("brief", brief_to_json(input.brief)),
    #("gate", json.nullable(input.gate, gate_outcome_to_json)),
    #("verdicts", json.array(input.verdicts, lens_verdict_to_json)),
    #("workspace_path", json.string(input.workspace_path)),
    #("gates", json.array(input.gates, gate_command_to_json)),
  ])
}

fn developer_input_decoder() -> decode.Decoder(DeveloperInput) {
  use brief <- decode.field("brief", brief_decoder())
  use gate <- decode.field("gate", decode.optional(gate_outcome_decoder()))
  use verdicts <- decode.field("verdicts", decode.list(lens_verdict_decoder()))
  use workspace_path <- decode.field("workspace_path", decode.string)
  use gates <- decode.optional_field(
    "gates",
    [],
    decode.list(gate_command_decoder()),
  )
  decode.success(DeveloperInput(
    brief: brief,
    gate: gate,
    verdicts: verdicts,
    workspace_path: workspace_path,
    gates: gates,
  ))
}

// --- review lenses -------------------------------------------------------------------

fn review_finding_to_json(finding: ReviewFinding) -> json.Json {
  json.object([
    #("severity", severity_to_json(finding.severity)),
    #("title", json.string(finding.title)),
    #("evidence", json.string(finding.evidence)),
  ])
}

fn review_finding_decoder() -> decode.Decoder(ReviewFinding) {
  use severity <- decode.field("severity", severity_decoder())
  use title <- decode.field("title", decode.string)
  use evidence <- decode.field("evidence", decode.string)
  decode.success(ReviewFinding(
    severity: severity,
    title: title,
    evidence: evidence,
  ))
}

fn lens_verdict_to_json(verdict: LensVerdict) -> json.Json {
  json.object([
    #("lens", json.string(verdict.lens)),
    #("findings", json.array(verdict.findings, review_finding_to_json)),
    #("overall", overall_to_json(verdict.overall)),
    #("reject_reason", json.nullable(verdict.reject_reason, json.string)),
  ])
}

fn lens_verdict_decoder() -> decode.Decoder(LensVerdict) {
  use lens <- decode.field("lens", decode.string)
  use findings <- decode.field(
    "findings",
    decode.list(review_finding_decoder()),
  )
  use overall <- decode.field("overall", overall_decoder())
  use reject_reason <- decode.optional_field(
    "reject_reason",
    option.None,
    decode.optional(decode.string),
  )
  decode.success(LensVerdict(
    lens: lens,
    findings: findings,
    overall: overall,
    reject_reason: reject_reason,
  ))
}

/// The lens agent's output codec (`lens-verdict.schema.json` mirror) — also
/// the `review_lens` child workflow's OUTPUT codec.
pub fn lens_verdict_codec() -> Codec(LensVerdict) {
  codec.json_codec(lens_verdict_to_json, lens_verdict_decoder())
}

/// The `review_lens` child workflow's input codec — also, verbatim, its
/// single agent activity's input codec.
pub fn lens_input_codec() -> Codec(LensInput) {
  codec.json_codec(lens_input_to_json, lens_input_decoder())
}

fn lens_input_to_json(input: LensInput) -> json.Json {
  json.object([
    #("lens", lens_to_json(input.lens)),
    #("brief", brief_to_json(input.brief)),
    #("diff", json.string(input.diff)),
    #("report", dev_report_to_json(input.report)),
    #("gate_runs", json.array(input.gate_runs, gate_command_run_to_json)),
  ])
}

fn lens_input_decoder() -> decode.Decoder(LensInput) {
  use lens <- decode.field("lens", lens_decoder())
  use brief <- decode.field("brief", brief_decoder())
  use diff <- decode.field("diff", decode.string)
  use report <- decode.field("report", dev_report_decoder())
  use gate_runs <- decode.field(
    "gate_runs",
    decode.list(gate_command_run_decoder()),
  )
  decode.success(LensInput(
    lens: lens,
    brief: brief,
    diff: diff,
    report: report,
    gate_runs: gate_runs,
  ))
}

// --- shell activity payloads ---------------------------------------------------

/// `provision_workspace` input codec.
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

/// `provision_workspace` output codec.
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

/// `run_gates` input codec.
pub fn gate_input_codec() -> Codec(GateInput) {
  codec.json_codec(gate_input_to_json, gate_input_decoder())
}

fn gate_input_to_json(input: GateInput) -> json.Json {
  json.object([
    #("workspace_path", json.string(input.workspace_path)),
    #("base_commit", json.string(input.base_commit)),
    #("gates", json.array(input.gates, gate_command_to_json)),
  ])
}

fn gate_input_decoder() -> decode.Decoder(GateInput) {
  use workspace_path <- decode.field("workspace_path", decode.string)
  use base_commit <- decode.field("base_commit", decode.string)
  use gates <- decode.field("gates", decode.list(gate_command_decoder()))
  decode.success(GateInput(
    workspace_path: workspace_path,
    base_commit: base_commit,
    gates: gates,
  ))
}

fn gate_command_run_to_json(run: GateCommandRun) -> json.Json {
  json.object([
    #("name", json.string(run.name)),
    #("exit_code", json.int(run.exit_code)),
    #("passed", json.bool(run.passed)),
    #("output_tail", json.string(run.output_tail)),
  ])
}

fn gate_command_run_decoder() -> decode.Decoder(GateCommandRun) {
  use name <- decode.field("name", decode.string)
  use exit_code <- decode.field("exit_code", decode.int)
  use passed <- decode.field("passed", decode.bool)
  use output_tail <- decode.field("output_tail", decode.string)
  decode.success(GateCommandRun(
    name: name,
    exit_code: exit_code,
    passed: passed,
    output_tail: output_tail,
  ))
}

/// `run_gates` output codec.
pub fn gate_outcome_codec() -> Codec(GateOutcome) {
  codec.json_codec(gate_outcome_to_json, gate_outcome_decoder())
}

fn gate_outcome_to_json(outcome: GateOutcome) -> json.Json {
  json.object([
    #("pass", json.bool(outcome.pass)),
    #("runs", json.array(outcome.runs, gate_command_run_to_json)),
    #("diff", json.string(outcome.diff)),
    #("diagnostics", json.string(outcome.diagnostics)),
  ])
}

fn gate_outcome_decoder() -> decode.Decoder(GateOutcome) {
  use pass <- decode.field("pass", decode.bool)
  use runs <- decode.field("runs", decode.list(gate_command_run_decoder()))
  use diff <- decode.field("diff", decode.string)
  use diagnostics <- decode.field("diagnostics", decode.string)
  decode.success(GateOutcome(
    pass: pass,
    runs: runs,
    diff: diff,
    diagnostics: diagnostics,
  ))
}

/// `cleanup_workspace` input codec.
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

/// `cleanup_workspace` output codec.
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

// --- pipeline result ------------------------------------------------------------------

/// The `dev_brief` workflow result codec.
pub fn brief_result_codec() -> Codec(BriefResult) {
  codec.json_codec(brief_result_to_json, brief_result_decoder())
}

fn brief_result_to_json(result: BriefResult) -> json.Json {
  json.object([
    #("brief_id", json.string(result.brief_id)),
    #("disposition", disposition_to_json(result.disposition)),
    #("fix_cycles", json.int(result.fix_cycles)),
    #("first_pass_accepted", json.bool(result.first_pass_accepted)),
    #("verdict_mismatches", strings(result.verdict_mismatches)),
    #("branch", json.string(result.branch)),
    #("report", json.nullable(result.report, dev_report_to_json)),
    #("gate", json.nullable(result.gate, gate_outcome_to_json)),
    #("verdicts", json.array(result.verdicts, lens_verdict_to_json)),
    #("workspace_removed", json.bool(result.workspace_removed)),
    #("summary", json.string(result.summary)),
  ])
}

fn brief_result_decoder() -> decode.Decoder(BriefResult) {
  use brief_id <- decode.field("brief_id", decode.string)
  use disposition <- decode.field("disposition", disposition_decoder())
  use fix_cycles <- decode.field("fix_cycles", decode.int)
  use first_pass_accepted <- decode.field("first_pass_accepted", decode.bool)
  use verdict_mismatches <- decode.field(
    "verdict_mismatches",
    decode.list(decode.string),
  )
  use branch <- decode.field("branch", decode.string)
  use report <- decode.field("report", decode.optional(dev_report_decoder()))
  use gate <- decode.field("gate", decode.optional(gate_outcome_decoder()))
  use verdicts <- decode.field("verdicts", decode.list(lens_verdict_decoder()))
  use workspace_removed <- decode.field("workspace_removed", decode.bool)
  use summary <- decode.field("summary", decode.string)
  decode.success(BriefResult(
    brief_id: brief_id,
    disposition: disposition,
    fix_cycles: fix_cycles,
    first_pass_accepted: first_pass_accepted,
    verdict_mismatches: verdict_mismatches,
    branch: branch,
    report: report,
    gate: gate,
    verdicts: verdicts,
    workspace_removed: workspace_removed,
    summary: summary,
  ))
}

// --- typed errors -----------------------------------------------------------------

/// The shared workflow error codec (`dev_brief` and `review_lens`).
pub fn dev_brief_error_codec() -> Codec(DevBriefError) {
  codec.json_codec(dev_brief_error_to_json, dev_brief_error_decoder())
}

fn dev_brief_error_to_json(error: DevBriefError) -> json.Json {
  case error {
    StageFailed(stage: stage, message: message) ->
      json.object([
        #("kind", json.string("stage_failed")),
        #("stage", json.string(stage)),
        #("message", json.string(message)),
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

fn dev_brief_error_decoder() -> decode.Decoder(DevBriefError) {
  use kind <- decode.field("kind", decode.string)
  case kind {
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
