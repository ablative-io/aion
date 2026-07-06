//// plan-fanout: a durable Aion workflow whose DEGREE OF PARALLELISM IS DECIDED
//// AT RUNTIME by a planner agent, not hardcoded.
////
//// Input: a design document (prospekt dev-cycle `design` kind — `title`,
//// `summary`, `requirements`, optional `non_goals`), plus optional runtime caps
//// (`max_units` default 8, `max_fix_rounds` default 2). The decoder is tolerant:
//// only the design core is required and extra fields ride along.
////
//// Flow:
////   1. PLANNER agent reads the document and emits a structured decomposition
////      `{units:[{unit_id,goal,inputs,depends_on}], rationale,
////      recommended_reviewers_per_unit}` — the planner decides N.
////   2. `validate_plan` (code, not agent) checks the DAG (unknown/duplicate ids,
////      cycles), clamps reviewers-per-unit to 1..3, enforces the unit-count cap,
////      and topologically LAYERS the units. A rejected plan short-circuits to a
////      terminal `rejected_plan` report — never a silent death.
////   3. For each dependency LAYER: fan out one DEV agent per unit IN PARALLEL
////      (`workflow.all`), then fan out M INDEPENDENT REVIEWER agents per unit IN
////      PARALLEL (distinct session each). `tally` (code) computes the MAJORITY
////      verdict; a blocked unit is re-dev'd in a BOUNDED fix round (cap
////      `max_fix_rounds`) then re-reviewed. A unit still blocked at the cap is
////      carried as blocked — the run still completes.
////   4. `integrate` (code) collects every unit's outcome into a structured run
////      report `{disposition, rationale, summary, units}`.
////
//// Every AGENT step (plan/dev/review) is routed to the worker's composed Norn
//// harness in DRIVEN mode and constrained by an `--output-schema`; every code
//// step (validate_plan/tally/integrate) is a plain registry activity whose logic
//// is unit-tested in the Rust worker. All activities dispatch on the
//// `plan-fanout` task queue so they never collide with other workers.
////
//// Data-flow note: payloads the workflow only shuttles between activities
//// (planner output, dev output, review output, the final report) are carried as
//// opaque `RawJson` — the workflow decodes only what it must branch on
//// (`validate_plan` and `tally` outputs). This keeps the Gleam codec surface
//// small while the agents and code steps see fully structured JSON.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list

/// The task queue every plan-fanout activity dispatches on. Distinct from the
/// `default` queue other example workers use, so dispatch never collides.
const task_queue = "plan-fanout"

/// Cap defaults, applied only when the input omits the override. Named, not bare
/// constants sprinkled through the logic.
const default_max_units = 8

const default_max_fix_rounds = 2

// --- Opaque pass-through payload -------------------------------------------

/// A payload the workflow only carries between activities, never inspects. Its
/// codec is the identity on the JSON string form: encode emits the stored JSON
/// verbatim, decode captures the whole payload. This lets planner/dev/review
/// outputs and the final report travel fully-structured without the workflow
/// modelling their shapes.
pub type RawJson {
  RawJson(json: String)
}

fn raw_json_codec() -> codec.Codec(RawJson) {
  codec.Codec(encode: fn(value: RawJson) { value.json }, decode: fn(input) {
    Ok(RawJson(input))
  })
}

// --- Decoded types the workflow branches on --------------------------------

/// One planned unit, decoded from the validated plan so the workflow can build
/// its dev input and honour its layer.
pub type PlanUnit {
  PlanUnit(
    unit_id: String,
    goal: String,
    inputs: List(String),
    depends_on: List(String),
  )
}

/// The `validate_plan` result: acceptance, the clamped reviewer count, the
/// fix-round cap, the topological layers, and the units. `accepted == False`
/// carries a human `reason` and empty layers/units.
pub type ValidatedPlan {
  ValidatedPlan(
    accepted: Bool,
    reason: String,
    rationale: String,
    reviewers_per_unit: Int,
    max_fix_rounds: Int,
    layers: List(List(String)),
    units: List(PlanUnit),
  )
}

/// One blocking defect with location evidence, as produced by reviewers and
/// aggregated by `tally`.
pub type Blocker {
  Blocker(file: String, line: Int, issue: String)
}

/// The `tally` result for one unit: the MAJORITY verdict plus the aggregated
/// blockers that justify it.
pub type Tally {
  Tally(
    unit_id: String,
    blocked: Bool,
    pass_count: Int,
    blocker_count: Int,
    blockers: List(Blocker),
  )
}

/// The settled outcome of one unit, accumulated for the integrate step.
pub type UnitResult {
  UnitResult(
    unit: PlanUnit,
    verdict: String,
    rounds_used: Int,
    dev_output: RawJson,
    blockers: List(Blocker),
  )
}

/// Typed workflow failure.
pub type WorkflowError {
  PlanFanoutFailed(message: String)
}

// --- Engine entry points ----------------------------------------------------

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  RawJson,
  RawJson,
  WorkflowError,
) {
  workflow.define(
    "plan_fanout",
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
      Error(PlanFanoutFailed("workflow input payload was not a JSON string"))
  }
}

/// The workflow body: plan -> validate -> layered fan-out -> integrate.
fn execute(design: RawJson) -> Result(RawJson, WorkflowError) {
  let #(max_units, default_fix_rounds) = decode_caps(design.json)

  // 1. PLANNER decides N.
  use plan <- result_then(dispatch(plan_activity(design.json, max_units)))

  // 2. Validate + layer (code).
  use validated <- result_then(
    dispatch_validated(validate_activity(
      plan.json,
      max_units,
      default_fix_rounds,
    )),
  )

  case validated.accepted {
    False -> Ok(rejected_report(validated.reason))
    True -> {
      // 3. Execute layers, fanning out dev + reviewers in parallel.
      use unit_results <- result_then(run_layers(validated))
      // 4. Integrate into the run report (code).
      let disposition = disposition_for(unit_results)
      dispatch(
        integrate_activity(build_integrate_input(
          validated,
          unit_results,
          disposition,
        )),
      )
    }
  }
}

// --- Layered execution ------------------------------------------------------

/// Run every layer in dependency order, threading settled unit results through.
/// Units within a layer run in parallel; a later layer starts once the earlier
/// ones have settled.
fn run_layers(plan: ValidatedPlan) -> Result(List(UnitResult), WorkflowError) {
  list.try_fold(plan.layers, [], fn(acc, layer_ids) {
    let layer_units = units_for_ids(plan.units, layer_ids)
    let pending = list.map(layer_units, fn(unit) { #(unit, []) })
    use layer_results <- result_then(run_round(pending, 0, plan))
    Ok(list.append(acc, layer_results))
  })
}

/// One dev+review+tally round over `pending` units (each paired with the prior
/// round's blockers, empty on the first round). Passed units settle; blocked
/// units recurse into a bounded fix round until the cap, after which they settle
/// as blocked so the run always completes.
fn run_round(
  pending: List(#(PlanUnit, List(Blocker))),
  round: Int,
  plan: ValidatedPlan,
) -> Result(List(UnitResult), WorkflowError) {
  let m = plan.reviewers_per_unit

  // DEV fan-out: one dev agent per pending unit, in parallel.
  let dev_activities =
    list.map(pending, fn(entry) {
      let #(unit, priors) = entry
      dev_activity(unit, priors, round)
    })
  use dev_outputs <- result_then(dispatch_all(dev_activities))

  // REVIEW fan-out: M independent reviewers per unit, all in parallel.
  let review_units = list.map(pending, fn(entry) { entry.0 })
  let unit_dev_pairs = list.zip(review_units, dev_outputs)
  let review_activities =
    list.flat_map(unit_dev_pairs, fn(pair) {
      let #(unit, dev_output) = pair
      list.map(indices(m), fn(reviewer_index) {
        review_activity(unit, dev_output, reviewer_index, round)
      })
    })
  use review_outputs <- result_then(dispatch_all(review_activities))
  let review_groups = list.sized_chunk(review_outputs, m)

  // TALLY: majority verdict per unit (code).
  use tallies <- result_then(
    list.zip(review_units, review_groups)
    |> list.try_map(fn(pair) {
      let #(unit, group) = pair
      dispatch_tally(tally_activity(unit.unit_id, group))
    }),
  )

  // Settle: partition passed vs blocked.
  let settled =
    zip3(review_units, dev_outputs, tallies)
    |> list.map(fn(triple) {
      let #(unit, dev_output, tally) = triple
      UnitResult(
        unit: unit,
        verdict: verdict_of(tally),
        rounds_used: round + 1,
        dev_output: dev_output,
        blockers: tally.blockers,
      )
    })
  let #(passed, blocked) =
    list.partition(settled, fn(result) { result.verdict == "pass" })

  case blocked, round < plan.max_fix_rounds {
    [], _ -> Ok(settled)
    _, False -> Ok(settled)
    _, True -> {
      let next_pending =
        list.map(blocked, fn(result) { #(result.unit, result.blockers) })
      use fixed <- result_then(run_round(next_pending, round + 1, plan))
      Ok(list.append(passed, fixed))
    }
  }
}

// --- Activity builders ------------------------------------------------------

/// The planner activity: reads the design document, decides N. AGENT step.
fn plan_activity(
  design_json: String,
  max_units: Int,
) -> activity.Activity(RawJson, RawJson) {
  let input =
    json.object([
      #("session_hint", json.string("plan")),
      #("max_units", json.int(max_units)),
      #("design_document", json.string(design_json)),
    ])
    |> json.to_string
  agent_activity("plan", input)
}

/// `validate_plan`: DAG + cap validation and topological layering. CODE step.
fn validate_activity(
  plan_json: String,
  max_units: Int,
  max_fix_rounds: Int,
) -> activity.Activity(RawJson, ValidatedPlan) {
  let input =
    json.object([
      #("plan", json.string(plan_json)),
      #("max_units", json.int(max_units)),
      #("max_fix_rounds", json.int(max_fix_rounds)),
    ])
    |> json.to_string
  activity.new(
    "validate_plan",
    RawJson(input),
    raw_json_codec(),
    validated_plan_codec(),
    fn(_) {
      Error(error.Terminal("validate_plan must run on a remote worker", ""))
    },
  )
  |> activity.task_queue(task_queue)
}

/// One dev agent for a unit. AGENT step. `priors` are the blockers from the
/// previous round (empty on the first round); `round` distinguishes the session.
fn dev_activity(
  unit: PlanUnit,
  priors: List(Blocker),
  round: Int,
) -> activity.Activity(RawJson, RawJson) {
  let input =
    json.object([
      #(
        "session_hint",
        json.string(unit.unit_id <> "-dev-r" <> int.to_string(round)),
      ),
      #("unit_id", json.string(unit.unit_id)),
      #("goal", json.string(unit.goal)),
      #("inputs", json.array(unit.inputs, json.string)),
      #("round", json.int(round)),
      #("prior_blockers", json.array(priors, blocker_to_json)),
    ])
    |> json.to_string
  agent_activity("dev", input)
}

/// One independent reviewer for a unit's dev output. AGENT step. The
/// `session_hint` makes every reviewer a distinct session — no shared context
/// beyond the unit goal and the dev output handed in here.
fn review_activity(
  unit: PlanUnit,
  dev_output: RawJson,
  reviewer_index: Int,
  round: Int,
) -> activity.Activity(RawJson, RawJson) {
  let hint =
    unit.unit_id
    <> "-review-r"
    <> int.to_string(round)
    <> "-"
    <> int.to_string(reviewer_index)
  let input =
    json.object([
      #("session_hint", json.string(hint)),
      #("unit_id", json.string(unit.unit_id)),
      #("goal", json.string(unit.goal)),
      #("reviewer_index", json.int(reviewer_index)),
      #("dev_output", json.string(dev_output.json)),
    ])
    |> json.to_string
  agent_activity("review", input)
}

/// `tally`: MAJORITY verdict over one unit's M reviews. CODE step.
fn tally_activity(
  unit_id: String,
  reviews: List(RawJson),
) -> activity.Activity(RawJson, Tally) {
  let input =
    json.object([
      #("unit_id", json.string(unit_id)),
      #("reviews", json.array(reviews, fn(review) { json.string(review.json) })),
    ])
    |> json.to_string
  activity.new("tally", RawJson(input), raw_json_codec(), tally_codec(), fn(_) {
    Error(error.Terminal("tally must run on a remote worker", ""))
  })
  |> activity.task_queue(task_queue)
}

/// `integrate`: collect every unit outcome into the run report. CODE step.
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

/// Common shape for an AGENT activity: opaque JSON in and out, on the
/// plan-fanout queue, routed to the worker's composed Norn harness by type.
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

// --- Integrate input + terminal reports -------------------------------------

/// Build the integrate activity input from the settled unit results.
fn build_integrate_input(
  plan: ValidatedPlan,
  results: List(UnitResult),
  disposition: String,
) -> String {
  json.object([
    #("disposition", json.string(disposition)),
    #("rationale", json.string(plan.rationale)),
    #("reviewers_per_unit", json.int(plan.reviewers_per_unit)),
    #("max_fix_rounds", json.int(plan.max_fix_rounds)),
    #("units", json.array(results, unit_result_to_json)),
  ])
  |> json.to_string
}

fn unit_result_to_json(result: UnitResult) -> json.Json {
  json.object([
    #("unit_id", json.string(result.unit.unit_id)),
    #("goal", json.string(result.unit.goal)),
    #("verdict", json.string(result.verdict)),
    #("rounds_used", json.int(result.rounds_used)),
    #("dev_output", json.string(result.dev_output.json)),
    #("blockers", json.array(result.blockers, blocker_to_json)),
  ])
}

/// The terminal report for a plan that failed validation. Built inline (no
/// integrate round-trip) because there is nothing to integrate — but it STILL
/// completes with the full report shape.
fn rejected_report(reason: String) -> RawJson {
  RawJson(
    json.object([
      #("disposition", json.string("rejected_plan")),
      #("rationale", json.string(reason)),
      #(
        "summary",
        json.object([
          #("unit_count", json.int(0)),
          #("passed", json.int(0)),
          #("blocked", json.int(0)),
          #("reviewers_per_unit", json.int(0)),
          #("max_fix_rounds", json.int(0)),
        ]),
      ),
      #("units", json.array([], json.string)),
    ])
    |> json.to_string,
  )
}

/// `completed` when every unit passed; `cap_exhausted` when any unit stayed
/// blocked after the fix-round cap. Either way the run completes with a report.
fn disposition_for(results: List(UnitResult)) -> String {
  case list.all(results, fn(result) { result.verdict == "pass" }) {
    True -> "completed"
    False -> "cap_exhausted"
  }
}

fn verdict_of(tally: Tally) -> String {
  case tally.blocked {
    True -> "blockers"
    False -> "pass"
  }
}

// --- Small helpers ----------------------------------------------------------

/// Decode the optional caps from the design document, applying the named
/// defaults when absent or unparseable.
fn decode_caps(design_json: String) -> #(Int, Int) {
  let decoder = {
    use max_units <- decode.optional_field(
      "max_units",
      default_max_units,
      decode.int,
    )
    use max_fix_rounds <- decode.optional_field(
      "max_fix_rounds",
      default_max_fix_rounds,
      decode.int,
    )
    decode.success(#(max_units, max_fix_rounds))
  }
  case json.parse(design_json, decoder) {
    Ok(caps) -> caps
    Error(_) -> #(default_max_units, default_max_fix_rounds)
  }
}

/// Resolve unit_ids to their PlanUnit records, preserving id order and dropping
/// ids validate_plan did not carry (validation guarantees they exist).
fn units_for_ids(units: List(PlanUnit), ids: List(String)) -> List(PlanUnit) {
  list.filter_map(ids, fn(id) {
    case list.find(units, fn(unit) { unit.unit_id == id }) {
      Ok(unit) -> Ok(unit)
      Error(_) -> Error(Nil)
    }
  })
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

fn dispatch_validated(
  activity: activity.Activity(RawJson, ValidatedPlan),
) -> Result(ValidatedPlan, WorkflowError) {
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
      Error(PlanFanoutFailed(activity_error_message(activity_error)))
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

// --- Codecs for the decoded types -------------------------------------------

fn blocker_to_json(blocker: Blocker) -> json.Json {
  json.object([
    #("file", json.string(blocker.file)),
    #("line", json.int(blocker.line)),
    #("issue", json.string(blocker.issue)),
  ])
}

fn blocker_decoder() -> decode.Decoder(Blocker) {
  use file <- decode.field("file", decode.string)
  use line <- decode.field("line", decode.int)
  use issue <- decode.field("issue", decode.string)
  decode.success(Blocker(file: file, line: line, issue: issue))
}

fn plan_unit_decoder() -> decode.Decoder(PlanUnit) {
  use unit_id <- decode.field("unit_id", decode.string)
  use goal <- decode.field("goal", decode.string)
  use inputs <- decode.field("inputs", decode.list(decode.string))
  use depends_on <- decode.field("depends_on", decode.list(decode.string))
  decode.success(PlanUnit(
    unit_id: unit_id,
    goal: goal,
    inputs: inputs,
    depends_on: depends_on,
  ))
}

fn validated_plan_codec() -> codec.Codec(ValidatedPlan) {
  codec.json_codec(validated_plan_to_json, validated_plan_decoder())
}

fn validated_plan_to_json(plan: ValidatedPlan) -> json.Json {
  json.object([
    #("accepted", json.bool(plan.accepted)),
    #("reason", json.string(plan.reason)),
    #("rationale", json.string(plan.rationale)),
    #("reviewers_per_unit", json.int(plan.reviewers_per_unit)),
    #("max_fix_rounds", json.int(plan.max_fix_rounds)),
    #(
      "layers",
      json.array(plan.layers, fn(layer) { json.array(layer, json.string) }),
    ),
    #("units", json.array(plan.units, plan_unit_to_json)),
  ])
}

fn plan_unit_to_json(unit: PlanUnit) -> json.Json {
  json.object([
    #("unit_id", json.string(unit.unit_id)),
    #("goal", json.string(unit.goal)),
    #("inputs", json.array(unit.inputs, json.string)),
    #("depends_on", json.array(unit.depends_on, json.string)),
  ])
}

fn validated_plan_decoder() -> decode.Decoder(ValidatedPlan) {
  use accepted <- decode.field("accepted", decode.bool)
  use reason <- decode.field("reason", decode.string)
  use rationale <- decode.field("rationale", decode.string)
  use reviewers_per_unit <- decode.field("reviewers_per_unit", decode.int)
  use max_fix_rounds <- decode.field("max_fix_rounds", decode.int)
  use layers <- decode.field("layers", decode.list(decode.list(decode.string)))
  use units <- decode.field("units", decode.list(plan_unit_decoder()))
  decode.success(ValidatedPlan(
    accepted: accepted,
    reason: reason,
    rationale: rationale,
    reviewers_per_unit: reviewers_per_unit,
    max_fix_rounds: max_fix_rounds,
    layers: layers,
    units: units,
  ))
}

fn tally_codec() -> codec.Codec(Tally) {
  codec.json_codec(tally_to_json, tally_decoder())
}

fn tally_to_json(tally: Tally) -> json.Json {
  json.object([
    #("unit_id", json.string(tally.unit_id)),
    #("blocked", json.bool(tally.blocked)),
    #("pass_count", json.int(tally.pass_count)),
    #("blocker_count", json.int(tally.blocker_count)),
    #("blockers", json.array(tally.blockers, blocker_to_json)),
  ])
}

fn tally_decoder() -> decode.Decoder(Tally) {
  use unit_id <- decode.field("unit_id", decode.string)
  use blocked <- decode.field("blocked", decode.bool)
  use pass_count <- decode.field("pass_count", decode.int)
  use blocker_count <- decode.field("blocker_count", decode.int)
  use blockers <- decode.field("blockers", decode.list(blocker_decoder()))
  decode.success(Tally(
    unit_id: unit_id,
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
    PlanFanoutFailed(message: message) ->
      json.object([#("plan_fanout_failed", json.string(message))])
  }
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use message <- decode.field("plan_fanout_failed", decode.string)
  decode.success(PlanFanoutFailed(message: message))
}
