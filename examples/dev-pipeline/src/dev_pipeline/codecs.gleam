//// JSON codecs for every value crossing the dev-pipeline workflow
//// boundaries: the brief-forge and implement-and-gate input/result/error
//// trios, the activity inputs/outputs, and the stage reports (scout report,
//// brief, refutation, implementation report), each mirroring its file in
//// `schemas/`.
////
//// Optional scalar fields encode by omission (never `null`); optional array
//// fields decode with an empty-list default and always encode — see the
//// header of `dev_pipeline/types` for the rationale.

import aion/codec
import dev_pipeline/types.{
  type AcceptanceGate, type Attack, type AttackOutcome, type AttackSeverity,
  type Brief, type BriefForgeError, type BriefForgeInput, type BriefForgeResult,
  type FileChange, type FixDesign, type ForgeOutcome, type GateAddressed,
  type GateAsserts, type GateAudit, type GateCliRun, type GateKind,
  type GateOutcome, type GateRecordEntry, type GateRun, type GateSpec,
  type ImplementAndGateError, type ImplementAndGateInput,
  type ImplementAndGateResult, type ImplementRound, type ImplementationReport,
  type Isolation, type ObservedBehavior, type Problem, type ProblemKind,
  type ProvisionInput, type Refutation, type RejectedAlternative,
  type RelevantFile, type ReportDeviation, type RootCause,
  type RootCauseHypothesis, type ScoutReport, type TeardownInput, type TornDown,
  type Workspace, Absence, AcceptanceGate, Attack, Brief, BriefForgeInput,
  BriefForgeResult, BriefForgeStageFailed, Bug, Clone, Command, Compatibility,
  Contested, Converged, Deflected, Design, Docs, Fatal, Feature, FileChange,
  FixDesign, GateAddressed, GateAudit, GateCliRun, GateRecordEntry, GateRun,
  GateSpec, GatesExhausted, GatesGreen, ImplementAndGateInput,
  ImplementAndGateResult, ImplementAndGateStageFailed, ImplementRound,
  ImplementationReport, Lands, LiveOperator, MustAddress, Note, ObservedBehavior,
  Outcome, OutcomeTest, Problem, ProvisionInput, Refactor, Refutation,
  RejectedAlternative, RelevantFile, ReportDeviation, RootCause,
  RootCauseHypothesis, ScoutReport, TeardownInput, TornDown, Withdrawn,
  Workspace, Worktree,
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

// --- agent prompt (shared activity input) -------------------------------------

/// Codec for the shared agent-activity input: the projected prompt TEXT
/// itself, encoded as a bare JSON string. The worker's driven-mode harness
/// unwraps the JSON-string payload and hands the inner text to the agent
/// verbatim — session identity, model, and output schema are harness spawn
/// arguments, never activity input.
pub fn prompt_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
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

// --- implement-and-gate: isolation and gate specs ------------------------------

/// Wire name for an `Isolation` mode.
pub fn isolation_to_string(isolation: Isolation) -> String {
  case isolation {
    Worktree -> "worktree"
    Clone -> "clone"
  }
}

fn isolation_decoder() -> decode.Decoder(Isolation) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "worktree" -> decode.success(Worktree)
      "clone" -> decode.success(Clone)
      _ -> decode.failure(Worktree, "worktree or clone")
    }
  })
}

fn gate_spec_to_json(gate: GateSpec) -> json.Json {
  json.object([
    #("id", json.string(gate.id)),
    #("command", json.string(gate.command)),
  ])
}

fn gate_spec_decoder() -> decode.Decoder(GateSpec) {
  use id <- decode.field("id", decode.string)
  use command <- decode.field("command", decode.string)
  decode.success(GateSpec(id: id, command: command))
}

// --- implement-and-gate: workflow input -----------------------------------------

/// Codec for `schemas/implement-and-gate.input.schema.json`. The embedded
/// brief rides through the typed `Brief` codec (schema-faithful field for
/// field); `node` and `implementer_model` are the two optional scalars.
pub fn implement_and_gate_input_codec() -> codec.Codec(ImplementAndGateInput) {
  codec.json_codec(
    implement_and_gate_input_to_json,
    implement_and_gate_input_decoder(),
  )
}

fn implement_and_gate_input_to_json(input: ImplementAndGateInput) -> json.Json {
  let fields = [
    #("brief", brief_to_json(input.brief)),
    #("repo_root", json.string(input.repo_root)),
    #("base_ref", json.string(input.base_ref)),
    #("isolation", json.string(isolation_to_string(input.isolation))),
  ]
  let fields = with_optional_string(fields, "node", input.node)
  let fields =
    list.append(fields, [
      #("fix_cap", json.int(input.fix_cap)),
      #("gates", json.array(input.gates, gate_spec_to_json)),
    ])
  json.object(with_optional_string(
    fields,
    "implementer_model",
    input.implementer_model,
  ))
}

fn implement_and_gate_input_decoder() -> decode.Decoder(ImplementAndGateInput) {
  use brief <- decode.field("brief", brief_decoder())
  use repo_root <- decode.field("repo_root", decode.string)
  use base_ref <- decode.field("base_ref", decode.string)
  use isolation <- decode.field("isolation", isolation_decoder())
  use node <- decode.optional_field("node", None, optional_string())
  use fix_cap <- decode.field("fix_cap", decode.int)
  use gates <- decode.field("gates", decode.list(gate_spec_decoder()))
  use implementer_model <- decode.optional_field(
    "implementer_model",
    None,
    optional_string(),
  )
  decode.success(ImplementAndGateInput(
    brief: brief,
    repo_root: repo_root,
    base_ref: base_ref,
    isolation: isolation,
    node: node,
    fix_cap: fix_cap,
    gates: gates,
    implementer_model: implementer_model,
  ))
}

// --- implement-and-gate: activity inputs/outputs --------------------------------

/// Codec for the `provision_workspace` activity input.
pub fn provision_input_codec() -> codec.Codec(ProvisionInput) {
  codec.json_codec(
    fn(input: ProvisionInput) {
      json.object([
        #("repo_root", json.string(input.repo_root)),
        #("base_ref", json.string(input.base_ref)),
        #("isolation", json.string(isolation_to_string(input.isolation))),
        #("task_ref", json.string(input.task_ref)),
      ])
    },
    {
      use repo_root <- decode.field("repo_root", decode.string)
      use base_ref <- decode.field("base_ref", decode.string)
      use isolation <- decode.field("isolation", isolation_decoder())
      use task_ref <- decode.field("task_ref", decode.string)
      decode.success(ProvisionInput(
        repo_root: repo_root,
        base_ref: base_ref,
        isolation: isolation,
        task_ref: task_ref,
      ))
    },
  )
}

/// Codec for the provisioned `Workspace`.
pub fn workspace_codec() -> codec.Codec(Workspace) {
  codec.json_codec(
    fn(workspace: Workspace) {
      json.object([#("path", json.string(workspace.path))])
    },
    {
      use path <- decode.field("path", decode.string)
      decode.success(Workspace(path: path))
    },
  )
}

/// Codec for the shared `ImplementRound` activity input (`implement` and
/// `implement_resume`): workspace, deterministic session id, projected
/// prompt, and the optional invocation-level model override (encoded by
/// omission — absence means the worker's pilot model).
pub fn implement_round_codec() -> codec.Codec(ImplementRound) {
  codec.json_codec(
    fn(round: ImplementRound) {
      let fields = [
        #("workspace_path", json.string(round.workspace_path)),
        #("session_id", json.string(round.session_id)),
        #("prompt", json.string(round.prompt)),
      ]
      json.object(with_optional_string(fields, "model", round.model))
    },
    {
      use workspace_path <- decode.field("workspace_path", decode.string)
      use session_id <- decode.field("session_id", decode.string)
      use prompt <- decode.field("prompt", decode.string)
      use model <- decode.optional_field("model", None, optional_string())
      decode.success(ImplementRound(
        workspace_path: workspace_path,
        session_id: session_id,
        prompt: prompt,
        model: model,
      ))
    },
  )
}

/// Codec for the `run_gate` activity input.
pub fn gate_run_codec() -> codec.Codec(GateRun) {
  codec.json_codec(
    fn(gate_run: GateRun) {
      json.object([
        #("workspace_path", json.string(gate_run.workspace_path)),
        #("gate_id", json.string(gate_run.gate_id)),
        #("command", json.string(gate_run.command)),
      ])
    },
    {
      use workspace_path <- decode.field("workspace_path", decode.string)
      use gate_id <- decode.field("gate_id", decode.string)
      use command <- decode.field("command", decode.string)
      decode.success(GateRun(
        workspace_path: workspace_path,
        gate_id: gate_id,
        command: command,
      ))
    },
  )
}

/// Codec for a completed gate command (`run_gate`'s output): the exit status
/// as DATA, the tail-bounded captured output, and the wall-clock duration.
pub fn gate_cli_run_codec() -> codec.Codec(GateCliRun) {
  codec.json_codec(
    fn(cli_run: GateCliRun) {
      json.object([
        #("exit_status", json.int(cli_run.exit_status)),
        #("output", json.string(cli_run.output)),
        #("duration_ms", json.int(cli_run.duration_ms)),
      ])
    },
    {
      use exit_status <- decode.field("exit_status", decode.int)
      use output <- decode.field("output", decode.string)
      use duration_ms <- decode.field("duration_ms", decode.int)
      decode.success(GateCliRun(
        exit_status: exit_status,
        output: output,
        duration_ms: duration_ms,
      ))
    },
  )
}

/// Codec for the `teardown_workspace` activity input.
pub fn teardown_input_codec() -> codec.Codec(TeardownInput) {
  codec.json_codec(
    fn(input: TeardownInput) {
      json.object([
        #("repo_root", json.string(input.repo_root)),
        #("workspace_path", json.string(input.workspace_path)),
      ])
    },
    {
      use repo_root <- decode.field("repo_root", decode.string)
      use workspace_path <- decode.field("workspace_path", decode.string)
      decode.success(TeardownInput(
        repo_root: repo_root,
        workspace_path: workspace_path,
      ))
    },
  )
}

/// Codec for `teardown_workspace`'s best-effort receipt.
pub fn torn_down_codec() -> codec.Codec(TornDown) {
  codec.json_codec(
    fn(torn_down: TornDown) {
      json.object([#("cleaned", json.bool(torn_down.cleaned))])
    },
    {
      use cleaned <- decode.field("cleaned", decode.bool)
      decode.success(TornDown(cleaned: cleaned))
    },
  )
}

// --- implementation report -------------------------------------------------------

/// Codec for `schemas/implementation-report.schema.json`. `new_tests` is the
/// schema's one optional array (default empty, always encoded).
pub fn implementation_report_codec() -> codec.Codec(ImplementationReport) {
  codec.json_codec(
    implementation_report_to_json,
    implementation_report_decoder(),
  )
}

fn implementation_report_to_json(report: ImplementationReport) -> json.Json {
  json.object([
    #("brief_ref", json.string(report.brief_ref)),
    #("summary", json.string(report.summary)),
    #("files_changed", json.array(report.files_changed, file_change_to_json)),
    #(
      "gates_addressed",
      json.array(report.gates_addressed, gate_addressed_to_json),
    ),
    #("deviations", json.array(report.deviations, report_deviation_to_json)),
    #("new_tests", string_list(report.new_tests)),
    #("concerns", string_list(report.concerns)),
    #("not_covered", string_list(report.not_covered)),
  ])
}

fn file_change_to_json(change: FileChange) -> json.Json {
  json.object([
    #("path", json.string(change.path)),
    #("change", json.string(change.change)),
  ])
}

fn gate_addressed_to_json(addressed: GateAddressed) -> json.Json {
  json.object([
    #("gate_id", json.string(addressed.gate_id)),
    #("how", json.string(addressed.how)),
  ])
}

fn report_deviation_to_json(deviation: ReportDeviation) -> json.Json {
  json.object([
    #("from", json.string(deviation.from)),
    #("to", json.string(deviation.to)),
    #("why", json.string(deviation.why)),
  ])
}

fn implementation_report_decoder() -> decode.Decoder(ImplementationReport) {
  use brief_ref <- decode.field("brief_ref", decode.string)
  use summary <- decode.field("summary", decode.string)
  use files_changed <- decode.field(
    "files_changed",
    decode.list(file_change_decoder()),
  )
  use gates_addressed <- decode.field(
    "gates_addressed",
    decode.list(gate_addressed_decoder()),
  )
  use deviations <- decode.field(
    "deviations",
    decode.list(report_deviation_decoder()),
  )
  use new_tests <- decode.optional_field(
    "new_tests",
    [],
    decode.list(decode.string),
  )
  use concerns <- decode.field("concerns", decode.list(decode.string))
  use not_covered <- decode.field("not_covered", decode.list(decode.string))
  decode.success(ImplementationReport(
    brief_ref: brief_ref,
    summary: summary,
    files_changed: files_changed,
    gates_addressed: gates_addressed,
    deviations: deviations,
    new_tests: new_tests,
    concerns: concerns,
    not_covered: not_covered,
  ))
}

fn file_change_decoder() -> decode.Decoder(FileChange) {
  use path <- decode.field("path", decode.string)
  use change <- decode.field("change", decode.string)
  decode.success(FileChange(path: path, change: change))
}

fn gate_addressed_decoder() -> decode.Decoder(GateAddressed) {
  use gate_id <- decode.field("gate_id", decode.string)
  use how <- decode.field("how", decode.string)
  decode.success(GateAddressed(gate_id: gate_id, how: how))
}

fn report_deviation_decoder() -> decode.Decoder(ReportDeviation) {
  use from <- decode.field("from", decode.string)
  use to <- decode.field("to", decode.string)
  use why <- decode.field("why", decode.string)
  decode.success(ReportDeviation(from: from, to: to, why: why))
}

// --- implement-and-gate: workflow result and error --------------------------------

fn gate_outcome_to_string(outcome: GateOutcome) -> String {
  case outcome {
    GatesGreen -> "gates_green"
    GatesExhausted -> "gates_exhausted"
  }
}

fn gate_outcome_decoder() -> decode.Decoder(GateOutcome) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "gates_green" -> decode.success(GatesGreen)
      "gates_exhausted" -> decode.success(GatesExhausted)
      _ -> decode.failure(GatesGreen, "gates_green or gates_exhausted")
    }
  })
}

fn gate_record_entry_to_json(entry: GateRecordEntry) -> json.Json {
  let fields = [
    #("id", json.string(entry.id)),
    #("command", json.string(entry.command)),
    #("exit_status", json.int(entry.exit_status)),
    #("duration_ms", json.int(entry.duration_ms)),
  ]
  json.object(with_optional_string(fields, "output_tail", entry.output_tail))
}

fn gate_record_entry_decoder() -> decode.Decoder(GateRecordEntry) {
  use id <- decode.field("id", decode.string)
  use command <- decode.field("command", decode.string)
  use exit_status <- decode.field("exit_status", decode.int)
  use duration_ms <- decode.field("duration_ms", decode.int)
  use output_tail <- decode.optional_field(
    "output_tail",
    None,
    optional_string(),
  )
  decode.success(GateRecordEntry(
    id: id,
    command: command,
    exit_status: exit_status,
    duration_ms: duration_ms,
    output_tail: output_tail,
  ))
}

/// Codec for `schemas/implement-and-gate.output.schema.json`: the outcome,
/// the implementer's last report, the final round's gate record, rounds
/// spent, and the (deliberately preserved) workspace path.
pub fn implement_and_gate_result_codec() -> codec.Codec(ImplementAndGateResult) {
  codec.json_codec(
    fn(result: ImplementAndGateResult) {
      json.object([
        #("outcome", json.string(gate_outcome_to_string(result.outcome))),
        #(
          "implementation_report",
          implementation_report_to_json(result.implementation_report),
        ),
        #(
          "gate_record",
          json.array(result.gate_record, gate_record_entry_to_json),
        ),
        #("rounds", json.int(result.rounds)),
        #("workspace_path", json.string(result.workspace_path)),
      ])
    },
    {
      use outcome <- decode.field("outcome", gate_outcome_decoder())
      use implementation_report <- decode.field(
        "implementation_report",
        implementation_report_decoder(),
      )
      use gate_record <- decode.field(
        "gate_record",
        decode.list(gate_record_entry_decoder()),
      )
      use rounds <- decode.field("rounds", decode.int)
      use workspace_path <- decode.field("workspace_path", decode.string)
      decode.success(ImplementAndGateResult(
        outcome: outcome,
        implementation_report: implementation_report,
        gate_record: gate_record,
        rounds: rounds,
        workspace_path: workspace_path,
      ))
    },
  )
}

/// Codec for the typed stage-failure error of implement-and-gate.
pub fn implement_and_gate_error_codec() -> codec.Codec(ImplementAndGateError) {
  codec.json_codec(
    fn(stage_error: ImplementAndGateError) {
      let ImplementAndGateStageFailed(stage: stage, message: message) =
        stage_error
      json.object([
        #("stage", json.string(stage)),
        #("message", json.string(message)),
      ])
    },
    {
      use stage <- decode.field("stage", decode.string)
      use message <- decode.field("message", decode.string)
      decode.success(ImplementAndGateStageFailed(stage: stage, message: message))
    },
  )
}
