//// JSON codecs for every value crossing the brief-forge workflow boundary:
//// the workflow input/result/error trio, the shared agent-round activity
//// input, and the three stage reports (scout report, brief, refutation),
//// each mirroring its file in `schemas/`.
////
//// Optional scalar fields encode by omission (never `null`); optional array
//// fields decode with an empty-list default and always encode — see the
//// header of `dev_pipeline/types` for the rationale.

import aion/codec
import dev_pipeline/types.{
  type AcceptanceGate, type Attack, type AttackOutcome, type AttackSeverity,
  type Brief, type BriefForgeError, type BriefForgeInput, type BriefForgeResult,
  type FixDesign, type ForgeOutcome, type GateAsserts, type GateAudit,
  type GateKind, type ObservedBehavior, type Problem, type ProblemKind,
  type Refutation, type RejectedAlternative, type RelevantFile, type RootCause,
  type RootCauseHypothesis, type ScoutReport, Absence, AcceptanceGate,
  AgentRound, Attack, Brief, BriefForgeInput, BriefForgeResult,
  BriefForgeStageFailed, Bug, Command, Compatibility, Contested, Converged,
  Deflected, Design, Docs, Fatal, Feature, FixDesign, GateAudit, Lands,
  LiveOperator, MustAddress, Note, ObservedBehavior, Outcome, OutcomeTest,
  Problem, Refactor, Refutation, RejectedAlternative, RelevantFile, RootCause,
  RootCauseHypothesis, ScoutReport, Withdrawn,
}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}

// --- shared helpers ---------------------------------------------------------

/// Append an optional string field to an encode list, encoding by omission.
fn with_optional_string(
  fields: List(#(String, json.Json)),
  name: String,
  value: Option(String),
) -> List(#(String, json.Json)) {
  case value {
    Some(text) -> list.append(fields, [#(name, json.string(text))])
    None -> fields
  }
}

fn string_list(values: List(String)) -> json.Json {
  json.array(values, json.string)
}

fn optional_string() -> decode.Decoder(Option(String)) {
  decode.map(decode.string, Some)
}

// --- workflow input ---------------------------------------------------------

/// Codec for `schemas/brief-forge.input.schema.json`. Every cap is required;
/// only `related_refs` and `emphases` are optional (default empty).
pub fn input_codec() -> codec.Codec(BriefForgeInput) {
  codec.json_codec(input_to_json, input_decoder())
}

fn input_to_json(input: BriefForgeInput) -> json.Json {
  json.object([
    #("task_statement", json.string(input.task_statement)),
    #("task_ref", json.string(input.task_ref)),
    #("repo_root", json.string(input.repo_root)),
    #("base_ref", json.string(input.base_ref)),
    #("related_refs", string_list(input.related_refs)),
    #("refute_cap", json.int(input.refute_cap)),
    #("diagnose_only", json.bool(input.diagnose_only)),
    #("emphases", string_list(input.emphases)),
  ])
}

fn input_decoder() -> decode.Decoder(BriefForgeInput) {
  use task_statement <- decode.field("task_statement", decode.string)
  use task_ref <- decode.field("task_ref", decode.string)
  use repo_root <- decode.field("repo_root", decode.string)
  use base_ref <- decode.field("base_ref", decode.string)
  use related_refs <- decode.optional_field(
    "related_refs",
    [],
    decode.list(decode.string),
  )
  use refute_cap <- decode.field("refute_cap", decode.int)
  use diagnose_only <- decode.field("diagnose_only", decode.bool)
  use emphases <- decode.optional_field(
    "emphases",
    [],
    decode.list(decode.string),
  )
  decode.success(BriefForgeInput(
    task_statement: task_statement,
    task_ref: task_ref,
    repo_root: repo_root,
    base_ref: base_ref,
    related_refs: related_refs,
    refute_cap: refute_cap,
    diagnose_only: diagnose_only,
    emphases: emphases,
  ))
}

// --- agent round (shared activity input) -------------------------------------

/// Codec for the shared `AgentRound` activity input: repo root, deterministic
/// session id, and the projected prompt.
pub fn agent_round_codec() -> codec.Codec(types.AgentRound) {
  codec.json_codec(
    fn(round: types.AgentRound) {
      json.object([
        #("repo_root", json.string(round.repo_root)),
        #("session_id", json.string(round.session_id)),
        #("prompt", json.string(round.prompt)),
      ])
    },
    {
      use repo_root <- decode.field("repo_root", decode.string)
      use session_id <- decode.field("session_id", decode.string)
      use prompt <- decode.field("prompt", decode.string)
      decode.success(AgentRound(
        repo_root: repo_root,
        session_id: session_id,
        prompt: prompt,
      ))
    },
  )
}

// --- scout report -------------------------------------------------------------

/// Codec for `schemas/scout-report.schema.json`.
pub fn scout_report_codec() -> codec.Codec(ScoutReport) {
  codec.json_codec(scout_report_to_json, scout_report_decoder())
}

/// Encoder for a `ScoutReport`, exported so prompts can render the report
/// verbatim into the design and refute rounds.
pub fn scout_report_to_json(report: ScoutReport) -> json.Json {
  json.object([
    #("subject", json.string(report.subject)),
    #(
      "relevant_files",
      json.array(report.relevant_files, relevant_file_to_json),
    ),
    #(
      "observed_behavior",
      json.array(report.observed_behavior, observed_behavior_to_json),
    ),
    #(
      "root_cause_hypotheses",
      json.array(report.root_cause_hypotheses, hypothesis_to_json),
    ),
    #("constraints", string_list(report.constraints)),
    #("prior_art", string_list(report.prior_art)),
    #("not_covered", string_list(report.not_covered)),
  ])
}

fn relevant_file_to_json(file: RelevantFile) -> json.Json {
  json.object([
    #("path", json.string(file.path)),
    #("role", json.string(file.role)),
    #("key_symbols", string_list(file.key_symbols)),
  ])
}

fn observed_behavior_to_json(behavior: ObservedBehavior) -> json.Json {
  json.object([
    #("claim", json.string(behavior.claim)),
    #("evidence", json.string(behavior.evidence)),
  ])
}

fn hypothesis_to_json(hypothesis: RootCauseHypothesis) -> json.Json {
  json.object([
    #("hypothesis", json.string(hypothesis.hypothesis)),
    #("supporting", json.string(hypothesis.supporting)),
    #("would_falsify", json.string(hypothesis.would_falsify)),
  ])
}

/// Decoder for a `ScoutReport`.
pub fn scout_report_decoder() -> decode.Decoder(ScoutReport) {
  use subject <- decode.field("subject", decode.string)
  use relevant_files <- decode.field(
    "relevant_files",
    decode.list(relevant_file_decoder()),
  )
  use observed_behavior <- decode.field(
    "observed_behavior",
    decode.list(observed_behavior_decoder()),
  )
  use root_cause_hypotheses <- decode.optional_field(
    "root_cause_hypotheses",
    [],
    decode.list(hypothesis_decoder()),
  )
  use constraints <- decode.field("constraints", decode.list(decode.string))
  use prior_art <- decode.optional_field(
    "prior_art",
    [],
    decode.list(decode.string),
  )
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(ScoutReport(
    subject: subject,
    relevant_files: relevant_files,
    observed_behavior: observed_behavior,
    root_cause_hypotheses: root_cause_hypotheses,
    constraints: constraints,
    prior_art: prior_art,
    not_covered: not_covered,
  ))
}

fn relevant_file_decoder() -> decode.Decoder(RelevantFile) {
  use path <- decode.field("path", decode.string)
  use role <- decode.field("role", decode.string)
  use key_symbols <- decode.optional_field(
    "key_symbols",
    [],
    decode.list(decode.string),
  )
  decode.success(RelevantFile(path: path, role: role, key_symbols: key_symbols))
}

fn observed_behavior_decoder() -> decode.Decoder(ObservedBehavior) {
  use claim <- decode.field("claim", decode.string)
  use evidence <- decode.field("evidence", decode.string)
  decode.success(ObservedBehavior(claim: claim, evidence: evidence))
}

fn hypothesis_decoder() -> decode.Decoder(RootCauseHypothesis) {
  use hypothesis <- decode.field("hypothesis", decode.string)
  use supporting <- decode.field("supporting", decode.string)
  use would_falsify <- decode.field("would_falsify", decode.string)
  decode.success(RootCauseHypothesis(
    hypothesis: hypothesis,
    supporting: supporting,
    would_falsify: would_falsify,
  ))
}

// --- brief -----------------------------------------------------------------

/// Codec for `schemas/brief.schema.json`.
pub fn brief_codec() -> codec.Codec(Brief) {
  codec.json_codec(brief_to_json, brief_decoder())
}

/// Wire name for a `ProblemKind`.
pub fn problem_kind_to_string(kind: ProblemKind) -> String {
  case kind {
    Bug -> "bug"
    Feature -> "feature"
    Refactor -> "refactor"
    Docs -> "docs"
    Design -> "design"
  }
}

/// Wire name for a `GateKind`.
pub fn gate_kind_to_string(kind: GateKind) -> String {
  case kind {
    Command -> "command"
    OutcomeTest -> "outcome_test"
    LiveOperator -> "live_operator"
  }
}

/// Wire name for a `GateAsserts` value — shape-assertions have no
/// constructor on purpose (rigid step 3).
pub fn gate_asserts_to_string(asserts: GateAsserts) -> String {
  case asserts {
    Outcome -> "outcome"
    Absence -> "absence"
    Compatibility -> "compatibility"
  }
}

/// Encoder for a `Brief`, exported so prompts can render the draft brief
/// verbatim into the refute round.
pub fn brief_to_json(brief: Brief) -> json.Json {
  let fields = [
    #("title", json.string(brief.title)),
    #("task_ref", json.string(brief.task_ref)),
    #("problem", problem_to_json(brief.problem)),
    #("fix_design", fix_design_to_json(brief.fix_design)),
    #(
      "acceptance_gates",
      json.array(brief.acceptance_gates, acceptance_gate_to_json),
    ),
    #("out_of_scope", string_list(brief.out_of_scope)),
  ]
  let fields =
    with_optional_string(
      fields,
      "refutation_survived",
      brief.refutation_survived,
    )
  json.object(
    list.append(fields, [#("not_covered", string_list(brief.not_covered))]),
  )
}

fn problem_to_json(problem: Problem) -> json.Json {
  let fields = [
    #("statement", json.string(problem.statement)),
    #("kind", json.string(problem_kind_to_string(problem.kind))),
  ]
  case problem.root_cause {
    Some(root_cause) ->
      json.object(
        list.append(fields, [#("root_cause", root_cause_to_json(root_cause))]),
      )
    None -> json.object(fields)
  }
}

fn root_cause_to_json(root_cause: RootCause) -> json.Json {
  json.object([
    #("statement", json.string(root_cause.statement)),
    #("causal_chain", string_list(root_cause.causal_chain)),
    #("evidence", json.string(root_cause.evidence)),
  ])
}

fn fix_design_to_json(fix_design: FixDesign) -> json.Json {
  json.object([
    #("approach", json.string(fix_design.approach)),
    #("touch_points", string_list(fix_design.touch_points)),
    #("invariants_to_preserve", string_list(fix_design.invariants_to_preserve)),
    #(
      "rejected_alternatives",
      json.array(fix_design.rejected_alternatives, rejected_alternative_to_json),
    ),
    #("risks", string_list(fix_design.risks)),
  ])
}

fn rejected_alternative_to_json(rejected: RejectedAlternative) -> json.Json {
  json.object([
    #("alternative", json.string(rejected.alternative)),
    #("why_rejected", json.string(rejected.why_rejected)),
  ])
}

fn acceptance_gate_to_json(gate: AcceptanceGate) -> json.Json {
  let fields = [
    #("id", json.string(gate.id)),
    #("statement", json.string(gate.statement)),
    #("kind", json.string(gate_kind_to_string(gate.kind))),
    #("asserts", json.string(gate_asserts_to_string(gate.asserts))),
  ]
  json.object(with_optional_string(fields, "command", gate.command))
}

/// Decoder for a `Brief`.
pub fn brief_decoder() -> decode.Decoder(Brief) {
  use title <- decode.field("title", decode.string)
  use task_ref <- decode.field("task_ref", decode.string)
  use problem <- decode.field("problem", problem_decoder())
  use fix_design <- decode.field("fix_design", fix_design_decoder())
  use acceptance_gates <- decode.field(
    "acceptance_gates",
    decode.list(acceptance_gate_decoder()),
  )
  use out_of_scope <- decode.field("out_of_scope", decode.list(decode.string))
  use refutation_survived <- decode.optional_field(
    "refutation_survived",
    None,
    optional_string(),
  )
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(Brief(
    title: title,
    task_ref: task_ref,
    problem: problem,
    fix_design: fix_design,
    acceptance_gates: acceptance_gates,
    out_of_scope: out_of_scope,
    refutation_survived: refutation_survived,
    not_covered: not_covered,
  ))
}

fn problem_kind_decoder() -> decode.Decoder(ProblemKind) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "bug" -> decode.success(Bug)
      "feature" -> decode.success(Feature)
      "refactor" -> decode.success(Refactor)
      "docs" -> decode.success(Docs)
      "design" -> decode.success(Design)
      _ -> decode.failure(Bug, "bug, feature, refactor, docs, or design")
    }
  })
}

fn problem_decoder() -> decode.Decoder(Problem) {
  use statement <- decode.field("statement", decode.string)
  use kind <- decode.field("kind", problem_kind_decoder())
  use root_cause <- decode.optional_field(
    "root_cause",
    None,
    decode.map(root_cause_decoder(), Some),
  )
  decode.success(Problem(
    statement: statement,
    kind: kind,
    root_cause: root_cause,
  ))
}

fn root_cause_decoder() -> decode.Decoder(RootCause) {
  use statement <- decode.field("statement", decode.string)
  use causal_chain <- decode.field("causal_chain", decode.list(decode.string))
  use evidence <- decode.field("evidence", decode.string)
  decode.success(RootCause(
    statement: statement,
    causal_chain: causal_chain,
    evidence: evidence,
  ))
}

fn fix_design_decoder() -> decode.Decoder(FixDesign) {
  use approach <- decode.field("approach", decode.string)
  use touch_points <- decode.field("touch_points", decode.list(decode.string))
  use invariants_to_preserve <- decode.optional_field(
    "invariants_to_preserve",
    [],
    decode.list(decode.string),
  )
  use rejected_alternatives <- decode.field(
    "rejected_alternatives",
    decode.list(rejected_alternative_decoder()),
  )
  use risks <- decode.optional_field("risks", [], decode.list(decode.string))
  decode.success(FixDesign(
    approach: approach,
    touch_points: touch_points,
    invariants_to_preserve: invariants_to_preserve,
    rejected_alternatives: rejected_alternatives,
    risks: risks,
  ))
}

fn rejected_alternative_decoder() -> decode.Decoder(RejectedAlternative) {
  use alternative <- decode.field("alternative", decode.string)
  use why_rejected <- decode.field("why_rejected", decode.string)
  decode.success(RejectedAlternative(
    alternative: alternative,
    why_rejected: why_rejected,
  ))
}

fn gate_kind_decoder() -> decode.Decoder(GateKind) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "command" -> decode.success(Command)
      "outcome_test" -> decode.success(OutcomeTest)
      "live_operator" -> decode.success(LiveOperator)
      _ -> decode.failure(Command, "command, outcome_test, or live_operator")
    }
  })
}

fn gate_asserts_decoder() -> decode.Decoder(GateAsserts) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "outcome" -> decode.success(Outcome)
      "absence" -> decode.success(Absence)
      "compatibility" -> decode.success(Compatibility)
      _ -> decode.failure(Outcome, "outcome, absence, or compatibility")
    }
  })
}

fn acceptance_gate_decoder() -> decode.Decoder(AcceptanceGate) {
  use id <- decode.field("id", decode.string)
  use statement <- decode.field("statement", decode.string)
  use kind <- decode.field("kind", gate_kind_decoder())
  use asserts <- decode.field("asserts", gate_asserts_decoder())
  use command <- decode.optional_field("command", None, optional_string())
  decode.success(AcceptanceGate(
    id: id,
    statement: statement,
    kind: kind,
    asserts: asserts,
    command: command,
  ))
}

// --- refutation ---------------------------------------------------------

/// Codec for `schemas/refutation.schema.json`.
pub fn refutation_codec() -> codec.Codec(Refutation) {
  codec.json_codec(refutation_to_json, refutation_decoder())
}

fn attack_outcome_to_string(outcome: AttackOutcome) -> String {
  case outcome {
    Lands -> "lands"
    Deflected -> "deflected"
    Withdrawn -> "withdrawn"
  }
}

fn attack_severity_to_string(severity: AttackSeverity) -> String {
  case severity {
    Fatal -> "fatal"
    MustAddress -> "must_address"
    Note -> "note"
  }
}

/// Encoder for a `Refutation`, exported so prompts can render a prior
/// refutation verbatim into the next design round.
pub fn refutation_to_json(refutation: Refutation) -> json.Json {
  json.object([
    #("design_survives", json.bool(refutation.design_survives)),
    #("attacks", json.array(refutation.attacks, attack_to_json)),
    #("gate_audit", gate_audit_to_json(refutation.gate_audit)),
    #("not_covered", string_list(refutation.not_covered)),
  ])
}

fn attack_to_json(attack: Attack) -> json.Json {
  let fields = [
    #("target", json.string(attack.target)),
    #("argument", json.string(attack.argument)),
  ]
  let fields = with_optional_string(fields, "evidence", attack.evidence)
  let fields =
    list.append(fields, [
      #("outcome", json.string(attack_outcome_to_string(attack.outcome))),
    ])
  case attack.severity_if_lands {
    Some(severity) ->
      json.object(
        list.append(fields, [
          #(
            "severity_if_lands",
            json.string(attack_severity_to_string(severity)),
          ),
        ]),
      )
    None -> json.object(fields)
  }
}

fn gate_audit_to_json(audit: GateAudit) -> json.Json {
  json.object([
    #("gates_assert_outcomes", json.bool(audit.gates_assert_outcomes)),
    #("holes", string_list(audit.holes)),
  ])
}

/// Decoder for a `Refutation`.
pub fn refutation_decoder() -> decode.Decoder(Refutation) {
  use design_survives <- decode.field("design_survives", decode.bool)
  use attacks <- decode.field("attacks", decode.list(attack_decoder()))
  use gate_audit <- decode.field("gate_audit", gate_audit_decoder())
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(Refutation(
    design_survives: design_survives,
    attacks: attacks,
    gate_audit: gate_audit,
    not_covered: not_covered,
  ))
}

fn attack_outcome_decoder() -> decode.Decoder(AttackOutcome) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "lands" -> decode.success(Lands)
      "deflected" -> decode.success(Deflected)
      "withdrawn" -> decode.success(Withdrawn)
      _ -> decode.failure(Lands, "lands, deflected, or withdrawn")
    }
  })
}

fn attack_severity_decoder() -> decode.Decoder(AttackSeverity) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "fatal" -> decode.success(Fatal)
      "must_address" -> decode.success(MustAddress)
      "note" -> decode.success(Note)
      _ -> decode.failure(Note, "fatal, must_address, or note")
    }
  })
}

fn attack_decoder() -> decode.Decoder(Attack) {
  use target <- decode.field("target", decode.string)
  use argument <- decode.field("argument", decode.string)
  use evidence <- decode.optional_field("evidence", None, optional_string())
  use outcome <- decode.field("outcome", attack_outcome_decoder())
  use severity_if_lands <- decode.optional_field(
    "severity_if_lands",
    None,
    decode.map(attack_severity_decoder(), Some),
  )
  decode.success(Attack(
    target: target,
    argument: argument,
    evidence: evidence,
    outcome: outcome,
    severity_if_lands: severity_if_lands,
  ))
}

fn gate_audit_decoder() -> decode.Decoder(GateAudit) {
  use gates_assert_outcomes <- decode.field(
    "gates_assert_outcomes",
    decode.bool,
  )
  use holes <- decode.field("holes", decode.list(decode.string))
  decode.success(GateAudit(
    gates_assert_outcomes: gates_assert_outcomes,
    holes: holes,
  ))
}

// --- workflow result and error ------------------------------------------------

fn forge_outcome_to_string(outcome: ForgeOutcome) -> String {
  case outcome {
    Converged -> "converged"
    Contested -> "contested"
  }
}

fn forge_outcome_decoder() -> decode.Decoder(ForgeOutcome) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "converged" -> decode.success(Converged)
      "contested" -> decode.success(Contested)
      _ -> decode.failure(Converged, "converged or contested")
    }
  })
}

/// Codec for `schemas/brief-forge.output.schema.json`: the brief verbatim,
/// the surviving/last refutation, rounds used, and `diagnose_only` passed
/// through.
pub fn result_codec() -> codec.Codec(BriefForgeResult) {
  codec.json_codec(
    fn(result: BriefForgeResult) {
      json.object([
        #("outcome", json.string(forge_outcome_to_string(result.outcome))),
        #("brief", brief_to_json(result.brief)),
        #("refutation", refutation_to_json(result.refutation)),
        #("rounds", json.int(result.rounds)),
        #("diagnose_only", json.bool(result.diagnose_only)),
      ])
    },
    {
      use outcome <- decode.field("outcome", forge_outcome_decoder())
      use brief <- decode.field("brief", brief_decoder())
      use refutation <- decode.field("refutation", refutation_decoder())
      use rounds <- decode.field("rounds", decode.int)
      use diagnose_only <- decode.field("diagnose_only", decode.bool)
      decode.success(BriefForgeResult(
        outcome: outcome,
        brief: brief,
        refutation: refutation,
        rounds: rounds,
        diagnose_only: diagnose_only,
      ))
    },
  )
}

/// Codec for the typed stage-failure error.
pub fn error_codec() -> codec.Codec(BriefForgeError) {
  codec.json_codec(
    fn(forge_error: BriefForgeError) {
      let BriefForgeStageFailed(stage: stage, message: message) = forge_error
      json.object([
        #("stage", json.string(stage)),
        #("message", json.string(message)),
      ])
    },
    {
      use stage <- decode.field("stage", decode.string)
      use message <- decode.field("message", decode.string)
      decode.success(BriefForgeStageFailed(stage: stage, message: message))
    },
  )
}
