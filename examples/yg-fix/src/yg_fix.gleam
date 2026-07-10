//// yg-fix: the FIX counterpart of the yg-review-salvage DIAGNOSIS workflow.
////
//// The diagnosis workflow fanned out adversarial verifiers over hundreds of
//// cached findings, confirmed the real ones, and synthesised a ranked work
//// list. This workflow consumes that confirmed-findings report and drives the
//// OTHER half: it fans out one fix agent per finding, has each fix
//// adversarially reviewed, and synthesises a fix report. The finding shape it
//// ingests is byte-compatible with the diagnosis output's `findings` array.
////
//// Input: `{ findings: [ {id,title,file,line,severity,category,detail,
//// recommendation} ], repo_root, max_findings?, max_reviewers?, max_fix_rounds? }`.
//// The decoder is tolerant — extra fields ride along.
////
//// Flow (every phase is a distinct, console-visible step — this doubles as a
//// stress test and an ops-console visualisation):
////   1. `ingest` (CODE) validates the report, caps the finding count, clamps
////      reviewers-per-fix to 1..3, and returns the accepted findings. An empty
////      or malformed report short-circuits to a terminal `rejected_input`
////      report — never a silent death.
////   2. FIX fan-out: one fix agent per finding IN PARALLEL (`workflow.all`).
////      Each reads the cited file and the finding and returns a structured
////      PROPOSED fix (a unified-diff patch + rationale) — it proposes, it does
////      not land; landing is a separate downstream step.
////   3. REVIEW fan-out: M INDEPENDENT adversarial reviewers per fix IN PARALLEL
////      (distinct session each), told to try to refute the fix. `tally` (CODE)
////      takes the MAJORITY verdict. A rejected fix is re-attempted in a BOUNDED
////      rework round (`max_fix_rounds`) with the reviewer blockers fed back,
////      then re-reviewed. Still-rejected at the cap settles as `rejected`.
////   4. `synthesize` (AGENT, high effort) turns the settled results into the
////      operator report + disposition table.
////   5. `integrate` (CODE) folds the synthesis into the final structured report
////      with by-severity / by-category / by-disposition rollups.
////
//// Every AGENT step (fix/review/synthesize) is routed to the worker's composed
//// Norn harness in DRIVEN mode and constrained by an `--output-schema`; every
//// CODE step (ingest/tally/integrate) is a plain registry activity whose logic
//// is unit-tested in the worker. All activities dispatch on the `yg-fix` task
//// queue. Payloads the workflow only shuttles between activities travel as
//// opaque `RawJson`; the workflow decodes only what it branches on (ingest and
//// tally outputs), exactly as plan-fanout does.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list

/// The task queue every yg-fix activity dispatches on. Distinct from other
/// example workers so dispatch never collides.
const task_queue = "yg-fix"

/// Working-default caps, applied only when the input omits the override. Named,
/// never bare constants sprinkled through the logic. `max_findings` keeps a
/// naive full-report run from fanning out into hundreds of concurrent agents —
/// slice the input to target a severity band or the top-N to go wider.
const default_max_findings = 25

const default_max_reviewers = 1

const default_max_fix_rounds = 1

// --- Opaque pass-through payload -------------------------------------------

/// A payload the workflow only carries between activities, never inspects. Its
/// codec is the identity on the JSON string form: encode emits the stored JSON
/// verbatim, decode captures the whole payload. Fix/review/synthesis outputs
/// travel fully-structured without the workflow modelling their shapes.
pub type RawJson {
  RawJson(json: String)
}

fn raw_json_codec() -> codec.Codec(RawJson) {
  codec.Codec(encode: fn(value: RawJson) { value.json }, decode: fn(input) {
    Ok(RawJson(input))
  })
}

// --- Decoded types the workflow branches on --------------------------------

/// One confirmed finding, decoded from the ingested report so the workflow can
/// build each fix agent's input.
pub type Finding {
  Finding(
    id: Int,
    title: String,
    file: String,
    line: Int,
    severity: String,
    category: String,
    detail: String,
    recommendation: String,
  )
}

/// The `ingest` result: acceptance, the repo root the fix agents operate in,
/// the clamped reviewer count and rework cap, and the accepted findings.
/// `accepted == False` carries a human `reason` and an empty finding list.
pub type Ingested {
  Ingested(
    accepted: Bool,
    reason: String,
    repo_root: String,
    reviewers_per_fix: Int,
    max_fix_rounds: Int,
    findings: List(Finding),
  )
}

/// One blocking defect a reviewer raised against a proposed fix, with evidence.
pub type Blocker {
  Blocker(issue: String, evidence: String)
}

/// The `tally` result for one fix: the MAJORITY verdict plus the aggregated
/// blockers that justify it.
pub type Tally {
  Tally(
    finding_id: String,
    blocked: Bool,
    pass_count: Int,
    blocker_count: Int,
    blockers: List(Blocker),
  )
}

/// The settled outcome of one finding, accumulated for synthesis + integrate.
pub type FixResult {
  FixResult(
    finding: Finding,
    verdict: String,
    rounds_used: Int,
    fix_output: RawJson,
    blockers: List(Blocker),
  )
}

/// Typed workflow failure.
pub type WorkflowError {
  YgFixFailed(message: String)
}

// --- Engine entry points ----------------------------------------------------

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(RawJson, RawJson, WorkflowError) {
  workflow.define(
    "yg_fix",
    raw_json_codec(),
    raw_json_codec(),
    workflow_error_codec(),
    execute,
  )
}

/// Engine entry: the runtime delivers the start input as a raw JSON string.
pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case execute(RawJson(raw_json)) {
        Ok(report) -> Ok(report.json)
        Error(workflow_error) -> Error(workflow_error)
      }
    Error(_) ->
      Error(YgFixFailed("workflow input payload was not a JSON string"))
  }
}

/// The workflow body: ingest -> fan-out fix+review -> synthesize -> integrate.
fn execute(input: RawJson) -> Result(RawJson, WorkflowError) {
  let #(max_findings, max_reviewers, max_fix_rounds) = decode_caps(input.json)

  // 1. Ingest + validate + cap (CODE).
  use ingested <- result_then(
    dispatch_ingested(ingest_activity(
      input.json,
      max_findings,
      max_reviewers,
      max_fix_rounds,
    )),
  )

  case ingested.accepted {
    False -> Ok(rejected_report(ingested.reason))
    True -> {
      // 2-3. Fan out fixes, review each, bounded rework.
      let pending = list.map(ingested.findings, fn(finding) { #(finding, []) })
      use fix_results <- result_then(run_round(pending, 0, ingested))
      // 4. Synthesize the operator report (AGENT, high effort).
      use synthesis <- result_then(
        dispatch(synthesize_activity(build_synthesize_input(
          ingested,
          fix_results,
        ))),
      )
      // 5. Fold synthesis into the final structured report (CODE).
      let disposition = disposition_for(fix_results)
      dispatch(
        integrate_activity(build_integrate_input(
          ingested,
          fix_results,
          synthesis,
          disposition,
        )),
      )
    }
  }
}

// --- Fan-out rounds ---------------------------------------------------------

/// One fix+review+tally round over `pending` findings (each paired with the
/// prior round's blockers, empty on the first round). Passed fixes settle;
/// rejected fixes recurse into a bounded rework round until the cap, after which
/// they settle as rejected so the run always completes.
fn run_round(
  pending: List(#(Finding, List(Blocker))),
  round: Int,
  ingested: Ingested,
) -> Result(List(FixResult), WorkflowError) {
  let m = ingested.reviewers_per_fix

  // FIX fan-out: one fix agent per pending finding, in parallel.
  let fix_activities =
    list.map(pending, fn(entry) {
      let #(finding, priors) = entry
      fix_activity(finding, ingested.repo_root, priors, round)
    })
  use fix_outputs <- result_then(dispatch_all(fix_activities))

  // REVIEW fan-out: M independent reviewers per fix, all in parallel.
  let findings = list.map(pending, fn(entry) { entry.0 })
  let finding_fix_pairs = list.zip(findings, fix_outputs)
  let review_activities =
    list.flat_map(finding_fix_pairs, fn(pair) {
      let #(finding, fix_output) = pair
      list.map(indices(m), fn(reviewer_index) {
        review_activity(finding, fix_output, reviewer_index, round)
      })
    })
  use review_outputs <- result_then(dispatch_all(review_activities))
  let review_groups = list.sized_chunk(review_outputs, m)

  // TALLY: majority verdict per fix (CODE).
  use tallies <- result_then(
    list.zip(findings, review_groups)
    |> list.try_map(fn(pair) {
      let #(finding, group) = pair
      dispatch_tally(tally_activity(int.to_string(finding.id), group))
    }),
  )

  // Settle: partition passed vs rejected.
  let settled =
    zip3(findings, fix_outputs, tallies)
    |> list.map(fn(triple) {
      let #(finding, fix_output, tally) = triple
      FixResult(
        finding: finding,
        verdict: verdict_of(tally),
        rounds_used: round + 1,
        fix_output: fix_output,
        blockers: tally.blockers,
      )
    })
  let #(passed, rejected) =
    list.partition(settled, fn(result) { result.verdict == "pass" })

  case rejected, round < ingested.max_fix_rounds {
    [], _ -> Ok(settled)
    _, False -> Ok(settled)
    _, True -> {
      let next_pending =
        list.map(rejected, fn(result) { #(result.finding, result.blockers) })
      use reworked <- result_then(run_round(next_pending, round + 1, ingested))
      Ok(list.append(passed, reworked))
    }
  }
}

// --- Activity builders ------------------------------------------------------

/// `ingest`: validate + cap the findings report and return accepted findings.
/// CODE step.
fn ingest_activity(
  input_json: String,
  max_findings: Int,
  max_reviewers: Int,
  max_fix_rounds: Int,
) -> activity.Activity(RawJson, Ingested) {
  let input =
    json.object([
      #("report", json.string(input_json)),
      #("max_findings", json.int(max_findings)),
      #("max_reviewers", json.int(max_reviewers)),
      #("max_fix_rounds", json.int(max_fix_rounds)),
    ])
    |> json.to_string
  activity.new(
    "ingest",
    RawJson(input),
    raw_json_codec(),
    ingested_codec(),
    fn(_) { Error(error.Terminal("ingest must run on a remote worker", "")) },
  )
  |> activity.task_queue(task_queue)
}

/// One fix agent for a finding. AGENT step. `priors` are the reviewer blockers
/// from the previous round (empty on the first round); `round` distinguishes the
/// session so a rework attempt resumes the same fix conversation.
fn fix_activity(
  finding: Finding,
  repo_root: String,
  priors: List(Blocker),
  round: Int,
) -> activity.Activity(RawJson, RawJson) {
  let input =
    json.object([
      #(
        "session_hint",
        json.string("fix-" <> int.to_string(finding.id) <> "-r" <> int.to_string(
          round,
        )),
      ),
      #("repo_root", json.string(repo_root)),
      #("round", json.int(round)),
      #("finding", finding_to_json(finding)),
      #("prior_blockers", json.array(priors, blocker_to_json)),
    ])
    |> json.to_string
  agent_activity("fix", input)
}

/// One independent reviewer for a proposed fix. AGENT step. The `session_hint`
/// makes every reviewer a distinct session — no shared context beyond the
/// finding and the fix output handed in here.
fn review_activity(
  finding: Finding,
  fix_output: RawJson,
  reviewer_index: Int,
  round: Int,
) -> activity.Activity(RawJson, RawJson) {
  let hint =
    "review-"
    <> int.to_string(finding.id)
    <> "-r"
    <> int.to_string(round)
    <> "-"
    <> int.to_string(reviewer_index)
  let input =
    json.object([
      #("session_hint", json.string(hint)),
      #("reviewer_index", json.int(reviewer_index)),
      #("finding", finding_to_json(finding)),
      #("fix_output", json.string(fix_output.json)),
    ])
    |> json.to_string
  agent_activity("review", input)
}

/// `tally`: MAJORITY verdict over one fix's M reviews. CODE step.
fn tally_activity(
  finding_id: String,
  reviews: List(RawJson),
) -> activity.Activity(RawJson, Tally) {
  let input =
    json.object([
      #("finding_id", json.string(finding_id)),
      #("reviews", json.array(reviews, fn(review) { json.string(review.json) })),
    ])
    |> json.to_string
  activity.new("tally", RawJson(input), raw_json_codec(), tally_codec(), fn(_) {
    Error(error.Terminal("tally must run on a remote worker", ""))
  })
  |> activity.task_queue(task_queue)
}

/// `synthesize`: turn the settled fix results into the operator report +
/// disposition table. AGENT step, high effort.
fn synthesize_activity(input: String) -> activity.Activity(RawJson, RawJson) {
  agent_activity("synthesize", input)
}

/// `integrate`: fold the synthesis into the final structured report with
/// rollups. CODE step.
fn integrate_activity(input: String) -> activity.Activity(RawJson, RawJson) {
  activity.new(
    "integrate",
    RawJson(input),
    raw_json_codec(),
    raw_json_codec(),
    fn(_) { Error(error.Terminal("integrate must run on a remote worker", "")) },
  )
  |> activity.task_queue(task_queue)
}

/// Common shape for an AGENT activity: opaque JSON in and out, on the yg-fix
/// queue, routed to the worker's composed Norn harness by type.
fn agent_activity(
  activity_type: String,
  input: String,
) -> activity.Activity(RawJson, RawJson) {
  activity.new(
    activity_type,
    RawJson(input),
    raw_json_codec(),
    raw_json_codec(),
    fn(_) {
      Error(error.Terminal(
        activity_type <> " is an agent activity; it must run on a remote worker",
        "",
      ))
    },
  )
  |> activity.task_queue(task_queue)
}

// --- Synthesize / integrate inputs + terminal report ------------------------

/// Build the synthesize agent input from the settled fix results.
fn build_synthesize_input(
  ingested: Ingested,
  results: List(FixResult),
) -> String {
  json.object([
    #("session_hint", json.string("synthesize")),
    #("repo_root", json.string(ingested.repo_root)),
    #("reviewers_per_fix", json.int(ingested.reviewers_per_fix)),
    #("max_fix_rounds", json.int(ingested.max_fix_rounds)),
    #("results", json.array(results, fix_result_to_json)),
  ])
  |> json.to_string
}

/// Build the integrate activity input: the settled results, the synthesis
/// output (carried raw), and the run disposition.
fn build_integrate_input(
  ingested: Ingested,
  results: List(FixResult),
  synthesis: RawJson,
  disposition: String,
) -> String {
  json.object([
    #("disposition", json.string(disposition)),
    #("repo_root", json.string(ingested.repo_root)),
    #("reviewers_per_fix", json.int(ingested.reviewers_per_fix)),
    #("max_fix_rounds", json.int(ingested.max_fix_rounds)),
    #("synthesis", json.string(synthesis.json)),
    #("results", json.array(results, fix_result_to_json)),
  ])
  |> json.to_string
}

fn fix_result_to_json(result: FixResult) -> json.Json {
  json.object([
    #("finding", finding_to_json(result.finding)),
    #("verdict", json.string(result.verdict)),
    #("rounds_used", json.int(result.rounds_used)),
    #("fix_output", json.string(result.fix_output.json)),
    #("blockers", json.array(result.blockers, blocker_to_json)),
  ])
}

/// The terminal report for an input that failed validation. Built inline (no
/// integrate round-trip) — but it STILL completes with the full report shape.
fn rejected_report(reason: String) -> RawJson {
  RawJson(
    json.object([
      #("disposition", json.string("rejected_input")),
      #("reason", json.string(reason)),
      #(
        "summary",
        json.object([
          #("total", json.int(0)),
          #("fixed", json.int(0)),
          #("rejected", json.int(0)),
          #("unaddressed", json.int(0)),
        ]),
      ),
      #("results", json.array([], json.string)),
    ])
    |> json.to_string,
  )
}

/// `completed` when every touched fix passed review; `cap_exhausted` when any
/// fix stayed rejected after the rework cap. Either way the run completes.
fn disposition_for(results: List(FixResult)) -> String {
  case list.all(results, fn(result) { result.verdict == "pass" }) {
    True -> "completed"
    False -> "cap_exhausted"
  }
}

fn verdict_of(tally: Tally) -> String {
  case tally.blocked {
    True -> "rejected"
    False -> "pass"
  }
}

// --- Small helpers ----------------------------------------------------------

/// Decode the optional caps from the input, applying the named defaults when
/// absent or unparseable.
fn decode_caps(input_json: String) -> #(Int, Int, Int) {
  let decoder = {
    use max_findings <- decode.optional_field(
      "max_findings",
      default_max_findings,
      decode.int,
    )
    use max_reviewers <- decode.optional_field(
      "max_reviewers",
      default_max_reviewers,
      decode.int,
    )
    use max_fix_rounds <- decode.optional_field(
      "max_fix_rounds",
      default_max_fix_rounds,
      decode.int,
    )
    decode.success(#(max_findings, max_reviewers, max_fix_rounds))
  }
  case json.parse(input_json, decoder) {
    Ok(caps) -> caps
    Error(_) -> #(
      default_max_findings,
      default_max_reviewers,
      default_max_fix_rounds,
    )
  }
}

/// `[0, 1, ..., count - 1]`. Replaces `list.range`, which this stdlib lacks.
fn indices(count: Int) -> List(Int) {
  do_indices(0, count, [])
}

fn do_indices(index: Int, count: Int, acc: List(Int)) -> List(Int) {
  case index >= count {
    True -> list.reverse(acc)
    False -> do_indices(index + 1, count, [index, ..acc])
  }
}

fn zip3(first: List(a), second: List(b), third: List(c)) -> List(#(a, b, c)) {
  list.zip(first, list.zip(second, third))
  |> list.map(fn(pair) {
    let #(a, #(b, c)) = pair
    #(a, b, c)
  })
}

/// `Result`-threading sugar so `use` reads left-to-right.
fn result_then(
  result: Result(a, WorkflowError),
  next: fn(a) -> Result(b, WorkflowError),
) -> Result(b, WorkflowError) {
  case result {
    Ok(value) -> next(value)
    Error(error) -> Error(error)
  }
}

// --- Dispatch wrappers (map ActivityError -> WorkflowError) -----------------

fn dispatch(
  activity: activity.Activity(RawJson, RawJson),
) -> Result(RawJson, WorkflowError) {
  workflow.run(activity) |> map_activity_error
}

fn dispatch_ingested(
  activity: activity.Activity(RawJson, Ingested),
) -> Result(Ingested, WorkflowError) {
  workflow.run(activity) |> map_activity_error
}

fn dispatch_tally(
  activity: activity.Activity(RawJson, Tally),
) -> Result(Tally, WorkflowError) {
  workflow.run(activity) |> map_activity_error
}

fn dispatch_all(
  activities: List(activity.Activity(RawJson, RawJson)),
) -> Result(List(RawJson), WorkflowError) {
  workflow.all(activities) |> map_activity_error
}

fn map_activity_error(
  result: Result(a, error.ActivityError),
) -> Result(a, WorkflowError) {
  case result {
    Ok(value) -> Ok(value)
    Error(activity_error) ->
      Error(YgFixFailed(activity_error_message(activity_error)))
  }
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(codec.DecodeError(reason: reason, path: _)) ->
      "activity result could not be decoded: " <> reason
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}

// --- Codecs -----------------------------------------------------------------

fn finding_to_json(finding: Finding) -> json.Json {
  json.object([
    #("id", json.int(finding.id)),
    #("title", json.string(finding.title)),
    #("file", json.string(finding.file)),
    #("line", json.int(finding.line)),
    #("severity", json.string(finding.severity)),
    #("category", json.string(finding.category)),
    #("detail", json.string(finding.detail)),
    #("recommendation", json.string(finding.recommendation)),
  ])
}

fn finding_decoder() -> decode.Decoder(Finding) {
  use id <- decode.field("id", decode.int)
  use title <- decode.field("title", decode.string)
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use severity <- decode.field("severity", decode.string)
  use category <- decode.field("category", decode.string)
  use detail <- decode.field("detail", decode.string)
  use recommendation <- decode.field("recommendation", decode.string)
  decode.success(Finding(
    id: id,
    title: title,
    file: file,
    line: line,
    severity: severity,
    category: category,
    detail: detail,
    recommendation: recommendation,
  ))
}

fn blocker_to_json(blocker: Blocker) -> json.Json {
  json.object([
    #("issue", json.string(blocker.issue)),
    #("evidence", json.string(blocker.evidence)),
  ])
}

fn blocker_decoder() -> decode.Decoder(Blocker) {
  use issue <- decode.field("issue", decode.string)
  use evidence <- decode.field("evidence", decode.string)
  decode.success(Blocker(issue: issue, evidence: evidence))
}

fn ingested_codec() -> codec.Codec(Ingested) {
  codec.json_codec(ingested_to_json, ingested_decoder())
}

fn ingested_to_json(ingested: Ingested) -> json.Json {
  json.object([
    #("accepted", json.bool(ingested.accepted)),
    #("reason", json.string(ingested.reason)),
    #("repo_root", json.string(ingested.repo_root)),
    #("reviewers_per_fix", json.int(ingested.reviewers_per_fix)),
    #("max_fix_rounds", json.int(ingested.max_fix_rounds)),
    #("findings", json.array(ingested.findings, finding_to_json)),
  ])
}

fn ingested_decoder() -> decode.Decoder(Ingested) {
  use accepted <- decode.field("accepted", decode.bool)
  use reason <- decode.field("reason", decode.string)
  use repo_root <- decode.field("repo_root", decode.string)
  use reviewers_per_fix <- decode.field("reviewers_per_fix", decode.int)
  use max_fix_rounds <- decode.field("max_fix_rounds", decode.int)
  use findings <- decode.field("findings", decode.list(finding_decoder()))
  decode.success(Ingested(
    accepted: accepted,
    reason: reason,
    repo_root: repo_root,
    reviewers_per_fix: reviewers_per_fix,
    max_fix_rounds: max_fix_rounds,
    findings: findings,
  ))
}

fn tally_codec() -> codec.Codec(Tally) {
  codec.json_codec(tally_to_json, tally_decoder())
}

fn tally_to_json(tally: Tally) -> json.Json {
  json.object([
    #("finding_id", json.string(tally.finding_id)),
    #("blocked", json.bool(tally.blocked)),
    #("pass_count", json.int(tally.pass_count)),
    #("blocker_count", json.int(tally.blocker_count)),
    #("blockers", json.array(tally.blockers, blocker_to_json)),
  ])
}

fn tally_decoder() -> decode.Decoder(Tally) {
  use finding_id <- decode.field("finding_id", decode.string)
  use blocked <- decode.field("blocked", decode.bool)
  use pass_count <- decode.field("pass_count", decode.int)
  use blocker_count <- decode.field("blocker_count", decode.int)
  use blockers <- decode.field("blockers", decode.list(blocker_decoder()))
  decode.success(Tally(
    finding_id: finding_id,
    blocked: blocked,
    pass_count: pass_count,
    blocker_count: blocker_count,
    blockers: blockers,
  ))
}

fn workflow_error_codec() -> codec.Codec(WorkflowError) {
  codec.json_codec(workflow_error_to_json, workflow_error_decoder())
}

fn workflow_error_to_json(err: WorkflowError) -> json.Json {
  case err {
    YgFixFailed(message: message) ->
      json.object([#("yg_fix_failed", json.string(message))])
  }
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use message <- decode.field("yg_fix_failed", decode.string)
  decode.success(YgFixFailed(message: message))
}
