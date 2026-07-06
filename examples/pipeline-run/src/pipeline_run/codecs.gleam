//// The JSON codecs binding every pipeline-run type to its wire shape.
////
//// Three kinds of contract live here:
////
//// 1. The parent input decoder for a prospekt `dev-cycle/brief` document —
////    tolerant of extra fields (prospekt injects `model`/`model_version`/slots
////    the workflow ignores), defaulting the two overridable caps.
//// 2. The four AGENT output codecs (`scout`/`stack_plan`/`dev`/`review`) — the
////    Gleam mirror of the norn `--output-schema` documents under `schemas/`;
////    the driven harness returns the schema-validated JSON as the activity
////    result and these decode it.
//// 3. The SHELL activity wire codecs (`provision`/`gate`/`land`/`notify`) plus
////    the child `pipeline_unit` I/O and the parent result — each byte-compatible
////    with the Rust worker's serde types (`worker/src/types.rs`).

import aion/codec.{type Codec}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option
import pipeline_run/types.{
  type Blocker, type DevReport, type Disposition, type GateInput,
  type GateOutcome, type LandInput, type LandOutcome, type LandUnit,
  type NotifyInput, type NotifyOutcome, type Observation, type PipelineBrief,
  type PipelineError, type PipelineResult, type PlanUnit, type ProvisionInput,
  type ReviewVerdict, type ScoutFindings, type StackPlan, type UnitInput,
  type UnitResult, type WorkspaceInfo, Blocker, DecodeInputFailed, DevReport,
  GateInput, GateOutcome, LandInput, LandOutcome, LandUnit, NotifyInput,
  NotifyOutcome, Observation, PipelineBrief, PipelineResult, PlanUnit,
  ProvisionInput, ReviewVerdict, ScoutFindings, StackFailed, StackInvalid,
  StackPlan, StageFailed, UnitInput, UnitResult, WorkspaceInfo,
}

// --- small shared helpers --------------------------------------------------

/// A codec for a bare JSON string — the agent-activity INPUT shape. In driven
/// mode the harness turns the JSON string back into the exact prompt text the
/// agent receives (see `aion-integration-norn`'s `prompt_from_spec`).
pub fn prompt_codec() -> Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn strings(values: List(String)) -> json.Json {
  json.array(values, json.string)
}

// --- Disposition -----------------------------------------------------------

fn disposition_to_json(disposition: Disposition) -> json.Json {
  json.string(types.disposition_to_string(disposition))
}

fn disposition_decoder() -> decode.Decoder(Disposition) {
  use tag <- decode.then(decode.string)
  case types.disposition_from_string(tag) {
    option.Some(disposition) -> decode.success(disposition)
    option.None -> decode.failure(types.Passed, "Disposition")
  }
}

// --- parent input: PipelineBrief -------------------------------------------

/// Decode the prospekt brief document into [`PipelineBrief`]. Unknown fields
/// (prospekt's injected `model`/`model_version`, execution slots) are ignored;
/// `constraints`, `state`, and the two caps default when absent.
pub fn brief_codec() -> Codec(PipelineBrief) {
  codec.json_codec(brief_to_json, brief_decoder())
}

fn brief_to_json(brief: PipelineBrief) -> json.Json {
  json.object([
    #("id", json.string(brief.id)),
    #("title", json.string(brief.title)),
    #("intent", json.string(brief.intent)),
    #(
      "scope",
      json.object([
        #("in", strings(brief.scope_in)),
        #("out", strings(brief.scope_out)),
      ]),
    ),
    #("acceptance_criteria", strings(brief.acceptance_criteria)),
    #("constraints", strings(brief.constraints)),
    #("state", json.string(brief.state)),
    #("repo_root", json.string(brief.repo_root)),
    #("base_branch", json.string(brief.base_branch)),
    #("dev_review_cap", json.int(brief.dev_review_cap)),
    #("gate_cap", json.int(brief.gate_cap)),
  ])
}

fn scope_field_decoder(key: String) -> decode.Decoder(List(String)) {
  decode.at(["scope", key], decode.list(decode.string))
}

fn brief_decoder() -> decode.Decoder(PipelineBrief) {
  use id <- decode.optional_field("id", "", decode.string)
  use title <- decode.field("title", decode.string)
  use intent <- decode.field("intent", decode.string)
  use scope_in <- decode.then(scope_field_decoder("in"))
  use scope_out <- decode.then(scope_field_decoder("out"))
  use acceptance_criteria <- decode.field(
    "acceptance_criteria",
    decode.list(decode.string),
  )
  use constraints <- decode.optional_field(
    "constraints",
    [],
    decode.list(decode.string),
  )
  use state <- decode.optional_field("state", "active", decode.string)
  use repo_root <- decode.field("repo_root", decode.string)
  use base_branch <- decode.field("base_branch", decode.string)
  use dev_review_cap <- decode.optional_field(
    "dev_review_cap",
    types.default_dev_review_cap(),
    decode.int,
  )
  use gate_cap <- decode.optional_field(
    "gate_cap",
    types.default_gate_cap(),
    decode.int,
  )
  decode.success(PipelineBrief(
    id: id,
    title: title,
    intent: intent,
    scope_in: scope_in,
    scope_out: scope_out,
    acceptance_criteria: acceptance_criteria,
    constraints: constraints,
    state: state,
    repo_root: repo_root,
    base_branch: base_branch,
    dev_review_cap: dev_review_cap,
    gate_cap: gate_cap,
  ))
}

// --- scout output ----------------------------------------------------------

pub fn scout_findings_codec() -> Codec(ScoutFindings) {
  codec.json_codec(scout_findings_to_json, scout_findings_decoder())
}

fn observation_to_json(observation: Observation) -> json.Json {
  json.object([
    #("location", json.string(observation.location)),
    #("note", json.string(observation.note)),
  ])
}

fn observation_decoder() -> decode.Decoder(Observation) {
  use location <- decode.field("location", decode.string)
  use note <- decode.field("note", decode.string)
  decode.success(Observation(location: location, note: note))
}

fn scout_findings_to_json(findings: ScoutFindings) -> json.Json {
  json.object([
    #("summary", json.string(findings.summary)),
    #("observations", json.array(findings.observations, observation_to_json)),
    #("integration_points", strings(findings.integration_points)),
    #("risks", strings(findings.risks)),
    #("not_covered", strings(findings.not_covered)),
  ])
}

fn scout_findings_decoder() -> decode.Decoder(ScoutFindings) {
  use summary <- decode.field("summary", decode.string)
  use observations <- decode.field(
    "observations",
    decode.list(observation_decoder()),
  )
  use integration_points <- decode.field(
    "integration_points",
    decode.list(decode.string),
  )
  use risks <- decode.field("risks", decode.list(decode.string))
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(ScoutFindings(
    summary: summary,
    observations: observations,
    integration_points: integration_points,
    risks: risks,
    not_covered: not_covered,
  ))
}

// --- plan output: the stack ------------------------------------------------

pub fn stack_plan_codec() -> Codec(StackPlan) {
  codec.json_codec(stack_plan_to_json, stack_plan_decoder())
}

fn plan_unit_to_json(unit: PlanUnit) -> json.Json {
  json.object([
    #("unit_id", json.string(unit.unit_id)),
    #("goal", json.string(unit.goal)),
    #("files_hint", strings(unit.files_hint)),
    #("depends_on", strings(unit.depends_on)),
  ])
}

fn plan_unit_decoder() -> decode.Decoder(PlanUnit) {
  use unit_id <- decode.field("unit_id", decode.string)
  use goal <- decode.field("goal", decode.string)
  use files_hint <- decode.field("files_hint", decode.list(decode.string))
  use depends_on <- decode.field("depends_on", decode.list(decode.string))
  decode.success(PlanUnit(
    unit_id: unit_id,
    goal: goal,
    files_hint: files_hint,
    depends_on: depends_on,
  ))
}

fn stack_plan_to_json(plan: StackPlan) -> json.Json {
  json.object([
    #("units", json.array(plan.units, plan_unit_to_json)),
    #("summary", json.string(plan.summary)),
    #("not_covered", strings(plan.not_covered)),
  ])
}

fn stack_plan_decoder() -> decode.Decoder(StackPlan) {
  use units <- decode.field("units", decode.list(plan_unit_decoder()))
  use summary <- decode.field("summary", decode.string)
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(StackPlan(
    units: units,
    summary: summary,
    not_covered: not_covered,
  ))
}

// --- dev output ------------------------------------------------------------

pub fn dev_report_codec() -> Codec(DevReport) {
  codec.json_codec(dev_report_to_json, dev_report_decoder())
}

fn dev_report_to_json(report: DevReport) -> json.Json {
  json.object([
    #("files_touched", strings(report.files_touched)),
    #("summary", json.string(report.summary)),
    #("not_covered", strings(report.not_covered)),
  ])
}

fn dev_report_decoder() -> decode.Decoder(DevReport) {
  use files_touched <- decode.field("files_touched", decode.list(decode.string))
  use summary <- decode.field("summary", decode.string)
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(DevReport(
    files_touched: files_touched,
    summary: summary,
    not_covered: not_covered,
  ))
}

// --- review output ---------------------------------------------------------

pub fn review_verdict_codec() -> Codec(ReviewVerdict) {
  codec.json_codec(review_verdict_to_json, review_verdict_decoder())
}

fn blocker_to_json(blocker: Blocker) -> json.Json {
  json.object([
    #("evidence", json.string(blocker.evidence)),
    #("problem", json.string(blocker.problem)),
    #("scenario", json.string(blocker.scenario)),
  ])
}

fn blocker_decoder() -> decode.Decoder(Blocker) {
  use evidence <- decode.field("evidence", decode.string)
  use problem <- decode.field("problem", decode.string)
  use scenario <- decode.field("scenario", decode.string)
  decode.success(Blocker(
    evidence: evidence,
    problem: problem,
    scenario: scenario,
  ))
}

fn review_verdict_to_json(verdict: ReviewVerdict) -> json.Json {
  json.object([
    #("pass", json.bool(verdict.pass)),
    #("blockers", json.array(verdict.blockers, blocker_to_json)),
    #("should_fix", strings(verdict.should_fix)),
    #("summary", json.string(verdict.summary)),
    #("not_covered", strings(verdict.not_covered)),
  ])
}

fn review_verdict_decoder() -> decode.Decoder(ReviewVerdict) {
  use pass <- decode.field("pass", decode.bool)
  use blockers <- decode.field("blockers", decode.list(blocker_decoder()))
  use should_fix <- decode.field("should_fix", decode.list(decode.string))
  use summary <- decode.field("summary", decode.string)
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(ReviewVerdict(
    pass: pass,
    blockers: blockers,
    should_fix: should_fix,
    summary: summary,
    not_covered: not_covered,
  ))
}

// --- shell activity: provision ---------------------------------------------

pub fn provision_input_codec() -> Codec(ProvisionInput) {
  codec.json_codec(provision_input_to_json, provision_input_decoder())
}

fn provision_input_to_json(input: ProvisionInput) -> json.Json {
  json.object([
    #("repo_root", json.string(input.repo_root)),
    #("base_branch", json.string(input.base_branch)),
    #("unit_branch", json.string(input.unit_branch)),
    #("workspace_path", json.string(input.workspace_path)),
  ])
}

fn provision_input_decoder() -> decode.Decoder(ProvisionInput) {
  use repo_root <- decode.field("repo_root", decode.string)
  use base_branch <- decode.field("base_branch", decode.string)
  use unit_branch <- decode.field("unit_branch", decode.string)
  use workspace_path <- decode.field("workspace_path", decode.string)
  decode.success(ProvisionInput(
    repo_root: repo_root,
    base_branch: base_branch,
    unit_branch: unit_branch,
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
  ])
}

fn workspace_info_decoder() -> decode.Decoder(WorkspaceInfo) {
  use workspace_path <- decode.field("workspace_path", decode.string)
  use branch <- decode.field("branch", decode.string)
  decode.success(WorkspaceInfo(workspace_path: workspace_path, branch: branch))
}

// --- shell activity: gate --------------------------------------------------

pub fn gate_input_codec() -> Codec(GateInput) {
  codec.json_codec(gate_input_to_json, gate_input_decoder())
}

fn gate_input_to_json(input: GateInput) -> json.Json {
  json.object([#("workspace_path", json.string(input.workspace_path))])
}

fn gate_input_decoder() -> decode.Decoder(GateInput) {
  use workspace_path <- decode.field("workspace_path", decode.string)
  decode.success(GateInput(workspace_path: workspace_path))
}

pub fn gate_outcome_codec() -> Codec(GateOutcome) {
  codec.json_codec(gate_outcome_to_json, gate_outcome_decoder())
}

fn gate_outcome_to_json(outcome: GateOutcome) -> json.Json {
  json.object([
    #("pass", json.bool(outcome.pass)),
    #("diagnostics", json.string(outcome.diagnostics)),
  ])
}

fn gate_outcome_decoder() -> decode.Decoder(GateOutcome) {
  use pass <- decode.field("pass", decode.bool)
  use diagnostics <- decode.field("diagnostics", decode.string)
  decode.success(GateOutcome(pass: pass, diagnostics: diagnostics))
}

// --- shell activity: land --------------------------------------------------

pub fn land_input_codec() -> Codec(LandInput) {
  codec.json_codec(land_input_to_json, land_input_decoder())
}

fn land_unit_to_json(unit: LandUnit) -> json.Json {
  json.object([
    #("unit_id", json.string(unit.unit_id)),
    #("branch", json.string(unit.branch)),
  ])
}

fn land_unit_decoder() -> decode.Decoder(LandUnit) {
  use unit_id <- decode.field("unit_id", decode.string)
  use branch <- decode.field("branch", decode.string)
  decode.success(LandUnit(unit_id: unit_id, branch: branch))
}

fn land_input_to_json(input: LandInput) -> json.Json {
  json.object([
    #("repo_root", json.string(input.repo_root)),
    #("base_branch", json.string(input.base_branch)),
    #("integration_branch", json.string(input.integration_branch)),
    #("units", json.array(input.units, land_unit_to_json)),
  ])
}

fn land_input_decoder() -> decode.Decoder(LandInput) {
  use repo_root <- decode.field("repo_root", decode.string)
  use base_branch <- decode.field("base_branch", decode.string)
  use integration_branch <- decode.field("integration_branch", decode.string)
  use units <- decode.field("units", decode.list(land_unit_decoder()))
  decode.success(LandInput(
    repo_root: repo_root,
    base_branch: base_branch,
    integration_branch: integration_branch,
    units: units,
  ))
}

pub fn land_outcome_codec() -> Codec(LandOutcome) {
  codec.json_codec(land_outcome_to_json, land_outcome_decoder())
}

fn land_outcome_to_json(outcome: LandOutcome) -> json.Json {
  json.object([
    #("landed", strings(outcome.landed)),
    #("integration_branch", json.string(outcome.integration_branch)),
    #("detail", json.string(outcome.detail)),
  ])
}

fn land_outcome_decoder() -> decode.Decoder(LandOutcome) {
  use landed <- decode.field("landed", decode.list(decode.string))
  use integration_branch <- decode.field("integration_branch", decode.string)
  use detail <- decode.field("detail", decode.string)
  decode.success(LandOutcome(
    landed: landed,
    integration_branch: integration_branch,
    detail: detail,
  ))
}

// --- shell activity: notify ------------------------------------------------

pub fn notify_input_codec() -> Codec(NotifyInput) {
  codec.json_codec(notify_input_to_json, notify_input_decoder())
}

fn notify_input_to_json(input: NotifyInput) -> json.Json {
  json.object([
    #("brief_id", json.string(input.brief_id)),
    #("summary", json.string(input.summary)),
  ])
}

fn notify_input_decoder() -> decode.Decoder(NotifyInput) {
  use brief_id <- decode.field("brief_id", decode.string)
  use summary <- decode.field("summary", decode.string)
  decode.success(NotifyInput(brief_id: brief_id, summary: summary))
}

pub fn notify_outcome_codec() -> Codec(NotifyOutcome) {
  codec.json_codec(notify_outcome_to_json, notify_outcome_decoder())
}

fn notify_outcome_to_json(outcome: NotifyOutcome) -> json.Json {
  json.object([
    #("sent", json.bool(outcome.sent)),
    #("detail", json.string(outcome.detail)),
  ])
}

fn notify_outcome_decoder() -> decode.Decoder(NotifyOutcome) {
  use sent <- decode.field("sent", decode.bool)
  use detail <- decode.field("detail", decode.string)
  decode.success(NotifyOutcome(sent: sent, detail: detail))
}

// --- child pipeline_unit I/O -----------------------------------------------

pub fn unit_input_codec() -> Codec(UnitInput) {
  codec.json_codec(unit_input_to_json, unit_input_decoder())
}

fn unit_input_to_json(input: UnitInput) -> json.Json {
  json.object([
    #("repo_root", json.string(input.repo_root)),
    #("base_branch", json.string(input.base_branch)),
    #("unit_branch", json.string(input.unit_branch)),
    #("unit_id", json.string(input.unit_id)),
    #("goal", json.string(input.goal)),
    #("files_hint", strings(input.files_hint)),
    #("brief_title", json.string(input.brief_title)),
    #("brief_intent", json.string(input.brief_intent)),
    #("acceptance_criteria", strings(input.acceptance_criteria)),
    #("constraints", strings(input.constraints)),
    #("scout_summary", json.string(input.scout_summary)),
    #("dev_review_cap", json.int(input.dev_review_cap)),
    #("gate_cap", json.int(input.gate_cap)),
  ])
}

fn unit_input_decoder() -> decode.Decoder(UnitInput) {
  use repo_root <- decode.field("repo_root", decode.string)
  use base_branch <- decode.field("base_branch", decode.string)
  use unit_branch <- decode.field("unit_branch", decode.string)
  use unit_id <- decode.field("unit_id", decode.string)
  use goal <- decode.field("goal", decode.string)
  use files_hint <- decode.field("files_hint", decode.list(decode.string))
  use brief_title <- decode.field("brief_title", decode.string)
  use brief_intent <- decode.field("brief_intent", decode.string)
  use acceptance_criteria <- decode.field(
    "acceptance_criteria",
    decode.list(decode.string),
  )
  use constraints <- decode.field("constraints", decode.list(decode.string))
  use scout_summary <- decode.field("scout_summary", decode.string)
  use dev_review_cap <- decode.field("dev_review_cap", decode.int)
  use gate_cap <- decode.field("gate_cap", decode.int)
  decode.success(UnitInput(
    repo_root: repo_root,
    base_branch: base_branch,
    unit_branch: unit_branch,
    unit_id: unit_id,
    goal: goal,
    files_hint: files_hint,
    brief_title: brief_title,
    brief_intent: brief_intent,
    acceptance_criteria: acceptance_criteria,
    constraints: constraints,
    scout_summary: scout_summary,
    dev_review_cap: dev_review_cap,
    gate_cap: gate_cap,
  ))
}

pub fn unit_result_codec() -> Codec(UnitResult) {
  codec.json_codec(unit_result_to_json, unit_result_decoder())
}

fn unit_result_to_json(result: UnitResult) -> json.Json {
  json.object([
    #("unit_id", json.string(result.unit_id)),
    #("branch", json.string(result.branch)),
    #("disposition", disposition_to_json(result.disposition)),
    #("dev_review_rounds", json.int(result.dev_review_rounds)),
    #("gate_rounds", json.int(result.gate_rounds)),
    #("last_review_summary", json.string(result.last_review_summary)),
    #("last_gate_diagnostics", json.string(result.last_gate_diagnostics)),
    #("files_touched", strings(result.files_touched)),
    #("summary", json.string(result.summary)),
  ])
}

fn unit_result_decoder() -> decode.Decoder(UnitResult) {
  use unit_id <- decode.field("unit_id", decode.string)
  use branch <- decode.field("branch", decode.string)
  use disposition <- decode.field("disposition", disposition_decoder())
  use dev_review_rounds <- decode.field("dev_review_rounds", decode.int)
  use gate_rounds <- decode.field("gate_rounds", decode.int)
  use last_review_summary <- decode.field("last_review_summary", decode.string)
  use last_gate_diagnostics <- decode.field(
    "last_gate_diagnostics",
    decode.string,
  )
  use files_touched <- decode.field("files_touched", decode.list(decode.string))
  use summary <- decode.field("summary", decode.string)
  decode.success(UnitResult(
    unit_id: unit_id,
    branch: branch,
    disposition: disposition,
    dev_review_rounds: dev_review_rounds,
    gate_rounds: gate_rounds,
    last_review_summary: last_review_summary,
    last_gate_diagnostics: last_gate_diagnostics,
    files_touched: files_touched,
    summary: summary,
  ))
}

// --- parent result ---------------------------------------------------------

pub fn pipeline_result_codec() -> Codec(PipelineResult) {
  codec.json_codec(pipeline_result_to_json, pipeline_result_decoder())
}

fn pipeline_result_to_json(result: PipelineResult) -> json.Json {
  json.object([
    #("disposition", disposition_to_json(result.disposition)),
    #("strata", json.array(result.strata, fn(stratum) { strings(stratum) })),
    #("units", json.array(result.units, unit_result_to_json)),
    #("landed", strings(result.landed)),
    #("summary", json.string(result.summary)),
  ])
}

fn pipeline_result_decoder() -> decode.Decoder(PipelineResult) {
  use disposition <- decode.field("disposition", disposition_decoder())
  use strata <- decode.field("strata", decode.list(decode.list(decode.string)))
  use units <- decode.field("units", decode.list(unit_result_decoder()))
  use landed <- decode.field("landed", decode.list(decode.string))
  use summary <- decode.field("summary", decode.string)
  decode.success(PipelineResult(
    disposition: disposition,
    strata: strata,
    units: units,
    landed: landed,
    summary: summary,
  ))
}

// --- error codec -----------------------------------------------------------

pub fn pipeline_error_codec() -> Codec(PipelineError) {
  codec.json_codec(pipeline_error_to_json, pipeline_error_decoder())
}

fn pipeline_error_to_json(error: PipelineError) -> json.Json {
  case error {
    StageFailed(stage: stage, message: message) ->
      json.object([
        #("kind", json.string("stage_failed")),
        #("stage", json.string(stage)),
        #("message", json.string(message)),
      ])
    StackInvalid(reason: reason) ->
      json.object([
        #("kind", json.string("stack_invalid")),
        #("reason", json.string(reason)),
      ])
    StackFailed(reason: reason) ->
      json.object([
        #("kind", json.string("stack_failed")),
        #("reason", json.string(reason)),
      ])
    DecodeInputFailed(message: message) ->
      json.object([
        #("kind", json.string("decode_input_failed")),
        #("message", json.string(message)),
      ])
  }
}

fn pipeline_error_decoder() -> decode.Decoder(PipelineError) {
  use kind <- decode.field("kind", decode.string)
  case kind {
    "stack_invalid" -> {
      use reason <- decode.field("reason", decode.string)
      decode.success(StackInvalid(reason: reason))
    }
    "stack_failed" -> {
      use reason <- decode.field("reason", decode.string)
      decode.success(StackFailed(reason: reason))
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

/// Render a list of `#(unit_id, branch)` pairs as [`LandUnit`] values for the
/// land activity input.
pub fn land_units(pairs: List(#(String, String))) -> List(LandUnit) {
  list.map(pairs, fn(pair) { LandUnit(unit_id: pair.0, branch: pair.1) })
}
