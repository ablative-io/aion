//// doc_certification: a design doc goes in; a pair-certified verdict comes out.
//// Two certifier halves review independently — agreement is only evidence when it
//// wasn't coordinated. Findings merge, the author revises in bounded rounds, and the
//// operator's signed ruling gates the finish. Evidence rides on every outcome.

import aion/activity
import aion/awl/codec as awlc
import aion/awl/error as awl_error
import aion/awl/runtime
import aion/codec.{type Codec}
import aion/duration
import aion/error
import aion/signal
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string

pub type DocRef {
  DocRef(
    repo: String,
    path: String,
    revision: String,
  )
}

pub type CertConfig {
  CertConfig(
    halves: List(String),
    max_revision_rounds: Int,
  )
}

/// Severity taxonomy for display; must_fix is the merge-driving bit.
pub type Severity {
  Blocking
  Major
  Minor
}

/// One certifier finding.
pub type Finding {
  Finding(
    half: String,
    severity: Severity,
    must_fix: Bool,
    summary: String,
    remedy: Option(String),
  )
}

pub type HalfReport {
  HalfReport(
    half: String,
    findings: List(Finding),
    approve: Bool,
  )
}

pub type Merged {
  Merged(
    all: List(Finding),
  )
}

pub type Round {
  Round(
    revision: String,
    summary: String,
    clean: Bool,
    remaining: List(Finding),
  )
}

pub type Ruling {
  Ruling(
    approved: Bool,
    note: Option(String),
  )
}

/// VM flow-typing cannot read a field of a `T?` binding (arm-local narrowing only),
/// so classifying the ruling costs a worker round-trip. See playtest log F15.
pub type Decision {
  Decision(
    signed: Bool,
    corrected: Bool,
  )
}

pub type PublishReceipt {
  PublishReceipt(
    location: String,
    revision: String,
  )
}

pub type Ack {
  Ack(
    ok: Bool,
  )
}

pub type Certified {
  Certified(
    doc_path: String,
    revision: String,
    rounds: Int,
    evidence: List(Finding),
  )
}

pub type Rejected {
  Rejected(
    doc_path: String,
    blocking: List(Finding),
  )
}

pub type Stalled {
  Stalled(
    reason: String,
    rounds_spent: Int,
  )
}

pub type DocCertificationInput {
  DocCertificationInput(
    doc: DocRef,
    config: CertConfig,
  )
}

pub type DocCertificationOutcome {
  CertifiedOutcome(Certified)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(DocCertificationInput, DocCertificationOutcome, awl_error.AwlError) {
  workflow.define(
    "doc_certification",
    doc_certification_input_codec(),
    doc_certification_outcome_codec(),
    awl_error.codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, awl_error.AwlError) {
  runtime.run(raw_input, doc_certification_input_codec(), doc_certification_outcome_codec(), execute)
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: DocCertificationInput) -> Result(DocCertificationOutcome, awl_error.AwlError) {
  let doc = input.doc
  let config = input.config
  step_independent_reviews(config, doc)
}

fn step_independent_reviews(config: CertConfig, doc: DocRef) -> Result(DocCertificationOutcome, awl_error.AwlError) {
  use reports <- result.try(workflow.map(config.halves, fn(half) { review_half_activity(doc, half) |> activity.retry(activity.RetryPolicy(max_attempts: 1, backoff: activity.Fixed(duration.milliseconds(60000)))) |> activity.timeout(duration.milliseconds(1800000)) |> activity.task_queue("certification") |> activity.node("reviewer") }) |> awl_error.map_activity_error)
  use merged <- result.try(merge_reports_activity(reports) |> activity.timeout(duration.milliseconds(300000)) |> activity.task_queue("certification") |> activity.node("shell") |> workflow.run |> awl_error.map_activity_error)
  let awl_piped_0 = list.filter(merged.all, fn(item) { item.must_fix })
  let blocking = awl_piped_0
  let awl_piped_0 = list.sort(merged.all, fn(left, right) { string.compare(left.half, right.half) })
  let evidence = awl_piped_0
  step_revision_rounds(blocking, config, doc, evidence)
}

fn step_revision_rounds(blocking: List(Finding), config: CertConfig, doc: DocRef, evidence: List(Finding)) -> Result(DocCertificationOutcome, awl_error.AwlError) {
  use #(round, rounds) <- result.try(revision_rounds_loop_0(Round(revision: doc.revision, summary: "", clean: False, remaining: blocking), 0, config.max_revision_rounds, doc))
  case round.clean {
    True -> {
      step_ruling_gate(blocking, config, doc, evidence, rounds)
    }
    False -> {
      Error(awl_error.AwlOutcomeFailure("rejected", json.to_string(rejected_to_json(Rejected(doc_path: doc.path, blocking: round.remaining)))))
    }
  }
}

fn step_ruling_gate(blocking: List(Finding), config: CertConfig, doc: DocRef, evidence: List(Finding), rounds: Int) -> Result(DocCertificationOutcome, awl_error.AwlError) {
  use ruling <- result.try(
    case workflow.with_timeout(fn() { workflow.receive(operator_ruling_signal()) |> awl_error.map_receive_error }, duration.milliseconds(172800000)) {
      Ok(value) -> Ok(Some(value))
      Error(error.TimedOutError(_)) -> Ok(None)
      Error(error.InnerError(inner)) -> Error(inner)
      Error(error.TimeoutEngineFailure(message)) -> Error(awl_error.AwlTimerFailed(message))
    },
  )
  use decision <- result.try(assess_ruling_activity(ruling) |> activity.timeout(duration.milliseconds(60000)) |> activity.task_queue("certification") |> activity.node("shell") |> workflow.run |> awl_error.map_activity_error)
  case decision.signed {
    True -> {
      step_finalize(doc, evidence, rounds)
    }
    False -> {
      case decision.corrected {
        True -> {
          step_revision_rounds(blocking, config, doc, evidence)
        }
        False -> {
          Error(awl_error.AwlOutcomeFailure("stalled", json.to_string(stalled_to_json(Stalled(reason: "operator ruling never arrived within 48h", rounds_spent: rounds)))))
        }
      }
    }
  }
}

fn step_finalize(doc: DocRef, evidence: List(Finding), rounds: Int) -> Result(DocCertificationOutcome, awl_error.AwlError) {
  let awl_attempt = fn() {
    use receipt <- result.try(publish_certification_activity(doc, evidence) |> activity.retry(activity.RetryPolicy(max_attempts: 2, backoff: activity.Fixed(duration.milliseconds(30000)))) |> activity.timeout(duration.milliseconds(300000)) |> activity.task_queue("certification") |> activity.node("shell") |> workflow.run |> awl_error.map_activity_error)
    use _ <- result.try(workflow.spawn("archive_certification", fn(_: json.Json) { Error(awl_error.AwlChildFailed("child workflow body runs in its own execution")) }, json.object([#("doc", doc_ref_to_json(doc)), #("receipt", publish_receipt_to_json(receipt))]), awlc.json_value(), ack_codec(), awl_error.codec()) |> awl_error.map_spawn_error)
    Ok(receipt)
  }
  case awl_attempt() {
    Ok(receipt) -> {
      Ok(CertifiedOutcome(Certified(doc_path: doc.path, revision: receipt.revision, rounds: rounds, evidence: evidence)))
    }
    Error(_) -> {
      use _ <- result.try(notify_operator_activity("certification publish failed for " <> doc.path) |> activity.timeout(duration.milliseconds(60000)) |> activity.task_queue("certification") |> activity.node("shell") |> workflow.run |> awl_error.map_activity_error)
      Error(awl_error.AwlOutcomeFailure("stalled", json.to_string(stalled_to_json(Stalled(reason: "publish failed after operator notify", rounds_spent: rounds)))))
    }
  }
}

fn revision_rounds_loop_0(round: Round, awl_count: Int, awl_max: Int, doc: DocRef) -> Result(#(Round, Int), awl_error.AwlError) {
  use round <- result.try(revise_round_activity(doc, round.remaining, round) |> activity.timeout(duration.milliseconds(2700000)) |> activity.task_queue("certification") |> activity.node("developer") |> workflow.run |> awl_error.map_activity_error)
  let awl_count = awl_count + 1
  case round.clean {
    True -> Ok(#(round, awl_count))
    False ->
      case awl_count >= awl_max {
        True -> Ok(#(round, awl_count))
        False -> revision_rounds_loop_0(round, awl_count, awl_max, doc)
      }
  }
}

pub type ReviewHalfInput {
  ReviewHalfInput(
    doc: DocRef,
    half: String,
  )
}

fn review_half_activity(
  doc: DocRef,
  half: String,
) -> activity.Activity(ReviewHalfInput, HalfReport) {
  activity.new(
    "review_half",
    ReviewHalfInput(
      doc: doc,
      half: half,
    ),
    review_half_input_codec(),
    half_report_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type MergeReportsInput {
  MergeReportsInput(
    reports: List(HalfReport),
  )
}

fn merge_reports_activity(
  reports: List(HalfReport),
) -> activity.Activity(MergeReportsInput, Merged) {
  activity.new(
    "merge_reports",
    MergeReportsInput(
      reports: reports,
    ),
    merge_reports_input_codec(),
    merged_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type ReviseRoundInput {
  ReviseRoundInput(
    doc: DocRef,
    findings: List(Finding),
    prior: Round,
  )
}

fn revise_round_activity(
  doc: DocRef,
  findings: List(Finding),
  prior: Round,
) -> activity.Activity(ReviseRoundInput, Round) {
  activity.new(
    "revise_round",
    ReviseRoundInput(
      doc: doc,
      findings: findings,
      prior: prior,
    ),
    revise_round_input_codec(),
    round_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type PublishCertificationInput {
  PublishCertificationInput(
    doc: DocRef,
    evidence: List(Finding),
  )
}

fn publish_certification_activity(
  doc: DocRef,
  evidence: List(Finding),
) -> activity.Activity(PublishCertificationInput, PublishReceipt) {
  activity.new(
    "publish_certification",
    PublishCertificationInput(
      doc: doc,
      evidence: evidence,
    ),
    publish_certification_input_codec(),
    publish_receipt_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type NotifyOperatorInput {
  NotifyOperatorInput(
    message: String,
  )
}

fn notify_operator_activity(
  message: String,
) -> activity.Activity(NotifyOperatorInput, Ack) {
  activity.new(
    "notify_operator",
    NotifyOperatorInput(
      message: message,
    ),
    notify_operator_input_codec(),
    ack_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type AssessRulingInput {
  AssessRulingInput(
    ruling: Option(Ruling),
  )
}

fn assess_ruling_activity(
  ruling: Option(Ruling),
) -> activity.Activity(AssessRulingInput, Decision) {
  activity.new(
    "assess_ruling",
    AssessRulingInput(
      ruling: ruling,
    ),
    assess_ruling_input_codec(),
    decision_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn operator_ruling_signal() -> signal.SignalRef(Ruling) {
  signal.new("operator_ruling", ruling_codec())
}

fn doc_certification_input_codec() -> Codec(DocCertificationInput) {
  codec.json_codec(doc_certification_input_to_json, doc_certification_input_decoder())
}

fn doc_certification_input_to_json(value: DocCertificationInput) -> json.Json {
  json.object([
    #("doc", doc_ref_to_json(value.doc)),
    #("config", cert_config_to_json(value.config)),
  ])
}

fn doc_certification_input_decoder() -> decode.Decoder(DocCertificationInput) {
  use doc <- decode.field("doc", doc_ref_decoder())
  use config <- decode.field("config", cert_config_decoder())
  decode.success(DocCertificationInput(
    doc: doc,
    config: config,
  ))
}

fn doc_certification_outcome_codec() -> Codec(DocCertificationOutcome) {
  codec.json_codec(doc_certification_outcome_to_json, doc_certification_outcome_decoder())
}

fn doc_certification_outcome_to_json(value: DocCertificationOutcome) -> json.Json {
  case value {
    CertifiedOutcome(payload) -> json.object([#("outcome", json.string("certified")), #("payload", certified_to_json(payload))])
  }
}

fn doc_certification_outcome_decoder() -> decode.Decoder(DocCertificationOutcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "certified" -> {
      use payload <- decode.field("payload", certified_decoder())
      decode.success(CertifiedOutcome(payload))
    }
    _ -> decode.failure(CertifiedOutcome(Certified(doc_path: "", revision: "", rounds: 0, evidence: [])), "DocCertificationOutcome")
  }
}

fn doc_ref_codec() -> Codec(DocRef) {
  codec.json_codec(doc_ref_to_json, doc_ref_decoder())
}

fn doc_ref_to_json(value: DocRef) -> json.Json {
  json.object([
    #("repo", awlc.string_to_json(value.repo)),
    #("path", awlc.string_to_json(value.path)),
    #("revision", awlc.string_to_json(value.revision)),
  ])
}

fn doc_ref_decoder() -> decode.Decoder(DocRef) {
  use repo <- decode.field("repo", awlc.string_decoder())
  use path <- decode.field("path", awlc.string_decoder())
  use revision <- decode.field("revision", awlc.string_decoder())
  decode.success(DocRef(
    repo: repo,
    path: path,
    revision: revision,
  ))
}

fn cert_config_codec() -> Codec(CertConfig) {
  codec.json_codec(cert_config_to_json, cert_config_decoder())
}

fn cert_config_to_json(value: CertConfig) -> json.Json {
  json.object([
    #("halves", list_string_to_json(value.halves)),
    #("max_revision_rounds", awlc.int_to_json(value.max_revision_rounds)),
  ])
}

fn cert_config_decoder() -> decode.Decoder(CertConfig) {
  use halves <- decode.field("halves", list_string_decoder())
  use max_revision_rounds <- decode.field("max_revision_rounds", awlc.int_decoder())
  decode.success(CertConfig(
    halves: halves,
    max_revision_rounds: max_revision_rounds,
  ))
}

fn severity_codec() -> Codec(Severity) {
  codec.json_codec(severity_to_json, severity_decoder())
}

fn severity_to_json(value: Severity) -> json.Json {
  case value {
    Blocking -> json.string("Blocking")
    Major -> json.string("Major")
    Minor -> json.string("Minor")
  }
}

fn severity_decoder() -> decode.Decoder(Severity) {
  use value <- decode.then(decode.string)
  case value {
    "Blocking" -> decode.success(Blocking)
    "Major" -> decode.success(Major)
    "Minor" -> decode.success(Minor)
    _ -> decode.failure(Blocking, "Severity")
  }
}

fn finding_codec() -> Codec(Finding) {
  codec.json_codec(finding_to_json, finding_decoder())
}

fn finding_to_json(value: Finding) -> json.Json {
  json.object(list.flatten([
    [#("half", awlc.string_to_json(value.half))],
    [#("severity", severity_to_json(value.severity))],
    [#("must_fix", awlc.bool_to_json(value.must_fix))],
    [#("summary", awlc.string_to_json(value.summary))],
    case value.remedy {
      Some(inner) -> [#("remedy", awlc.string_to_json(inner))]
      None -> []
    },
  ]))
}

fn finding_decoder() -> decode.Decoder(Finding) {
  use half <- decode.field("half", awlc.string_decoder())
  use severity <- decode.field("severity", severity_decoder())
  use must_fix <- decode.field("must_fix", awlc.bool_decoder())
  use summary <- decode.field("summary", awlc.string_decoder())
  use remedy <- decode.optional_field("remedy", None, decode.map(awlc.string_decoder(), Some))
  decode.success(Finding(
    half: half,
    severity: severity,
    must_fix: must_fix,
    summary: summary,
    remedy: remedy,
  ))
}

fn half_report_codec() -> Codec(HalfReport) {
  codec.json_codec(half_report_to_json, half_report_decoder())
}

fn half_report_to_json(value: HalfReport) -> json.Json {
  json.object([
    #("half", awlc.string_to_json(value.half)),
    #("findings", list_finding_to_json(value.findings)),
    #("approve", awlc.bool_to_json(value.approve)),
  ])
}

fn half_report_decoder() -> decode.Decoder(HalfReport) {
  use half <- decode.field("half", awlc.string_decoder())
  use findings <- decode.field("findings", list_finding_decoder())
  use approve <- decode.field("approve", awlc.bool_decoder())
  decode.success(HalfReport(
    half: half,
    findings: findings,
    approve: approve,
  ))
}

fn merged_codec() -> Codec(Merged) {
  codec.json_codec(merged_to_json, merged_decoder())
}

fn merged_to_json(value: Merged) -> json.Json {
  json.object([
    #("all", list_finding_to_json(value.all)),
  ])
}

fn merged_decoder() -> decode.Decoder(Merged) {
  use all <- decode.field("all", list_finding_decoder())
  decode.success(Merged(
    all: all,
  ))
}

fn round_codec() -> Codec(Round) {
  codec.json_codec(round_to_json, round_decoder())
}

fn round_to_json(value: Round) -> json.Json {
  json.object([
    #("revision", awlc.string_to_json(value.revision)),
    #("summary", awlc.string_to_json(value.summary)),
    #("clean", awlc.bool_to_json(value.clean)),
    #("remaining", list_finding_to_json(value.remaining)),
  ])
}

fn round_decoder() -> decode.Decoder(Round) {
  use revision <- decode.field("revision", awlc.string_decoder())
  use summary <- decode.field("summary", awlc.string_decoder())
  use clean <- decode.field("clean", awlc.bool_decoder())
  use remaining <- decode.field("remaining", list_finding_decoder())
  decode.success(Round(
    revision: revision,
    summary: summary,
    clean: clean,
    remaining: remaining,
  ))
}

fn ruling_codec() -> Codec(Ruling) {
  codec.json_codec(ruling_to_json, ruling_decoder())
}

fn ruling_to_json(value: Ruling) -> json.Json {
  json.object(list.flatten([
    [#("approved", awlc.bool_to_json(value.approved))],
    case value.note {
      Some(inner) -> [#("note", awlc.string_to_json(inner))]
      None -> []
    },
  ]))
}

fn ruling_decoder() -> decode.Decoder(Ruling) {
  use approved <- decode.field("approved", awlc.bool_decoder())
  use note <- decode.optional_field("note", None, decode.map(awlc.string_decoder(), Some))
  decode.success(Ruling(
    approved: approved,
    note: note,
  ))
}

fn decision_codec() -> Codec(Decision) {
  codec.json_codec(decision_to_json, decision_decoder())
}

fn decision_to_json(value: Decision) -> json.Json {
  json.object([
    #("signed", awlc.bool_to_json(value.signed)),
    #("corrected", awlc.bool_to_json(value.corrected)),
  ])
}

fn decision_decoder() -> decode.Decoder(Decision) {
  use signed <- decode.field("signed", awlc.bool_decoder())
  use corrected <- decode.field("corrected", awlc.bool_decoder())
  decode.success(Decision(
    signed: signed,
    corrected: corrected,
  ))
}

fn publish_receipt_codec() -> Codec(PublishReceipt) {
  codec.json_codec(publish_receipt_to_json, publish_receipt_decoder())
}

fn publish_receipt_to_json(value: PublishReceipt) -> json.Json {
  json.object([
    #("location", awlc.string_to_json(value.location)),
    #("revision", awlc.string_to_json(value.revision)),
  ])
}

fn publish_receipt_decoder() -> decode.Decoder(PublishReceipt) {
  use location <- decode.field("location", awlc.string_decoder())
  use revision <- decode.field("revision", awlc.string_decoder())
  decode.success(PublishReceipt(
    location: location,
    revision: revision,
  ))
}

fn ack_codec() -> Codec(Ack) {
  codec.json_codec(ack_to_json, ack_decoder())
}

fn ack_to_json(value: Ack) -> json.Json {
  json.object([
    #("ok", awlc.bool_to_json(value.ok)),
  ])
}

fn ack_decoder() -> decode.Decoder(Ack) {
  use ok <- decode.field("ok", awlc.bool_decoder())
  decode.success(Ack(
    ok: ok,
  ))
}

fn certified_codec() -> Codec(Certified) {
  codec.json_codec(certified_to_json, certified_decoder())
}

fn certified_to_json(value: Certified) -> json.Json {
  json.object([
    #("doc_path", awlc.string_to_json(value.doc_path)),
    #("revision", awlc.string_to_json(value.revision)),
    #("rounds", awlc.int_to_json(value.rounds)),
    #("evidence", list_finding_to_json(value.evidence)),
  ])
}

fn certified_decoder() -> decode.Decoder(Certified) {
  use doc_path <- decode.field("doc_path", awlc.string_decoder())
  use revision <- decode.field("revision", awlc.string_decoder())
  use rounds <- decode.field("rounds", awlc.int_decoder())
  use evidence <- decode.field("evidence", list_finding_decoder())
  decode.success(Certified(
    doc_path: doc_path,
    revision: revision,
    rounds: rounds,
    evidence: evidence,
  ))
}

fn rejected_codec() -> Codec(Rejected) {
  codec.json_codec(rejected_to_json, rejected_decoder())
}

fn rejected_to_json(value: Rejected) -> json.Json {
  json.object([
    #("doc_path", awlc.string_to_json(value.doc_path)),
    #("blocking", list_finding_to_json(value.blocking)),
  ])
}

fn rejected_decoder() -> decode.Decoder(Rejected) {
  use doc_path <- decode.field("doc_path", awlc.string_decoder())
  use blocking <- decode.field("blocking", list_finding_decoder())
  decode.success(Rejected(
    doc_path: doc_path,
    blocking: blocking,
  ))
}

fn stalled_codec() -> Codec(Stalled) {
  codec.json_codec(stalled_to_json, stalled_decoder())
}

fn stalled_to_json(value: Stalled) -> json.Json {
  json.object([
    #("reason", awlc.string_to_json(value.reason)),
    #("rounds_spent", awlc.int_to_json(value.rounds_spent)),
  ])
}

fn stalled_decoder() -> decode.Decoder(Stalled) {
  use reason <- decode.field("reason", awlc.string_decoder())
  use rounds_spent <- decode.field("rounds_spent", awlc.int_decoder())
  decode.success(Stalled(
    reason: reason,
    rounds_spent: rounds_spent,
  ))
}

fn review_half_input_codec() -> Codec(ReviewHalfInput) {
  codec.json_codec(review_half_input_to_json, review_half_input_decoder())
}

fn review_half_input_to_json(value: ReviewHalfInput) -> json.Json {
  json.object([
    #("doc", doc_ref_to_json(value.doc)),
    #("half", awlc.string_to_json(value.half)),
  ])
}

fn review_half_input_decoder() -> decode.Decoder(ReviewHalfInput) {
  use doc <- decode.field("doc", doc_ref_decoder())
  use half <- decode.field("half", awlc.string_decoder())
  decode.success(ReviewHalfInput(
    doc: doc,
    half: half,
  ))
}

fn merge_reports_input_codec() -> Codec(MergeReportsInput) {
  codec.json_codec(merge_reports_input_to_json, merge_reports_input_decoder())
}

fn merge_reports_input_to_json(value: MergeReportsInput) -> json.Json {
  json.object([
    #("reports", list_half_report_to_json(value.reports)),
  ])
}

fn merge_reports_input_decoder() -> decode.Decoder(MergeReportsInput) {
  use reports <- decode.field("reports", list_half_report_decoder())
  decode.success(MergeReportsInput(
    reports: reports,
  ))
}

fn revise_round_input_codec() -> Codec(ReviseRoundInput) {
  codec.json_codec(revise_round_input_to_json, revise_round_input_decoder())
}

fn revise_round_input_to_json(value: ReviseRoundInput) -> json.Json {
  json.object([
    #("doc", doc_ref_to_json(value.doc)),
    #("findings", list_finding_to_json(value.findings)),
    #("prior", round_to_json(value.prior)),
  ])
}

fn revise_round_input_decoder() -> decode.Decoder(ReviseRoundInput) {
  use doc <- decode.field("doc", doc_ref_decoder())
  use findings <- decode.field("findings", list_finding_decoder())
  use prior <- decode.field("prior", round_decoder())
  decode.success(ReviseRoundInput(
    doc: doc,
    findings: findings,
    prior: prior,
  ))
}

fn publish_certification_input_codec() -> Codec(PublishCertificationInput) {
  codec.json_codec(publish_certification_input_to_json, publish_certification_input_decoder())
}

fn publish_certification_input_to_json(value: PublishCertificationInput) -> json.Json {
  json.object([
    #("doc", doc_ref_to_json(value.doc)),
    #("evidence", list_finding_to_json(value.evidence)),
  ])
}

fn publish_certification_input_decoder() -> decode.Decoder(PublishCertificationInput) {
  use doc <- decode.field("doc", doc_ref_decoder())
  use evidence <- decode.field("evidence", list_finding_decoder())
  decode.success(PublishCertificationInput(
    doc: doc,
    evidence: evidence,
  ))
}

fn notify_operator_input_codec() -> Codec(NotifyOperatorInput) {
  codec.json_codec(notify_operator_input_to_json, notify_operator_input_decoder())
}

fn notify_operator_input_to_json(value: NotifyOperatorInput) -> json.Json {
  json.object([
    #("message", awlc.string_to_json(value.message)),
  ])
}

fn notify_operator_input_decoder() -> decode.Decoder(NotifyOperatorInput) {
  use message <- decode.field("message", awlc.string_decoder())
  decode.success(NotifyOperatorInput(
    message: message,
  ))
}

fn assess_ruling_input_codec() -> Codec(AssessRulingInput) {
  codec.json_codec(assess_ruling_input_to_json, assess_ruling_input_decoder())
}

fn assess_ruling_input_to_json(value: AssessRulingInput) -> json.Json {
  json.object(list.flatten([
    case value.ruling {
      Some(inner) -> [#("ruling", ruling_to_json(inner))]
      None -> []
    },
  ]))
}

fn assess_ruling_input_decoder() -> decode.Decoder(AssessRulingInput) {
  use ruling <- decode.optional_field("ruling", None, decode.map(ruling_decoder(), Some))
  decode.success(AssessRulingInput(
    ruling: ruling,
  ))
}

fn list_finding_codec() -> Codec(List(Finding)) {
  codec.json_codec(list_finding_to_json, list_finding_decoder())
}
fn list_finding_to_json(values: List(Finding)) -> json.Json { json.array(values, finding_to_json) }
fn list_finding_decoder() -> decode.Decoder(List(Finding)) { decode.list(finding_decoder()) }

fn list_half_report_codec() -> Codec(List(HalfReport)) {
  codec.json_codec(list_half_report_to_json, list_half_report_decoder())
}
fn list_half_report_to_json(values: List(HalfReport)) -> json.Json { json.array(values, half_report_to_json) }
fn list_half_report_decoder() -> decode.Decoder(List(HalfReport)) { decode.list(half_report_decoder()) }

fn list_string_codec() -> Codec(List(String)) {
  codec.json_codec(list_string_to_json, list_string_decoder())
}
fn list_string_to_json(values: List(String)) -> json.Json { json.array(values, awlc.string_to_json) }
fn list_string_decoder() -> decode.Decoder(List(String)) { decode.list(awlc.string_decoder()) }

fn option_ruling_codec() -> Codec(Option(Ruling)) {
  codec.json_codec(option_ruling_to_json, option_ruling_decoder())
}
fn option_ruling_to_json(value: Option(Ruling)) -> json.Json { json.nullable(value, ruling_to_json) }
fn option_ruling_decoder() -> decode.Decoder(Option(Ruling)) { decode.optional(ruling_decoder()) }

fn option_string_codec() -> Codec(Option(String)) {
  codec.json_codec(option_string_to_json, option_string_decoder())
}
fn option_string_to_json(value: Option(String)) -> json.Json { json.nullable(value, awlc.string_to_json) }
fn option_string_decoder() -> decode.Decoder(Option(String)) { decode.optional(awlc.string_decoder()) }

