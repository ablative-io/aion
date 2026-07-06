//// Prospekt -> Aion bridge demo: a prospekt-validated `incident` document
//// drives an Aion workflow as typed input.
////
//// The workflow accepts the effective incident document JSON minted and
//// ready-checked by `prospekt` (the debug-loop model's `incident` kind),
//// decodes it into a typed `Incident` record, schedules ONE remote `triage`
//// activity, and returns the typed `TriageSummary` the worker produces.
////
//// This is the whole point of the bridge: typed-structured-input in, typed
//// structured-output out. No timers, no fan-out, no entropy — the `triage`
//// activity maps severity to a next action with plain string logic.
////
//// The document carries prospekt-injected fields the workflow does not need
//// (`model`, `model_version`, and the `forensics` slot). JSON decoding ignores
//// unknown fields, so those ride along harmlessly; the workflow reads only the
//// effective payload plus `id` and lifecycle `state`.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/option.{type Option, None, Some}

/// Where the incident happened. Mirrors the incident schema's `environment`
/// object: `binary` and `invocation` are required, `model` (the LLM in play, if
/// any) is optional.
pub type Environment {
  Environment(binary: String, model: Option(String), invocation: String)
}

/// The typed incident, decoded from the prospekt effective document. Only the
/// fields the triage step reads are modelled; prospekt's injected `model`,
/// `model_version`, and `forensics` slot are ignored by the decoder.
pub type Incident {
  Incident(
    id: String,
    title: String,
    severity: String,
    observed: String,
    expected: String,
    environment: Environment,
    state: String,
  )
}

/// The structured triage result the workflow returns.
pub type TriageSummary {
  TriageSummary(
    incident_id: String,
    severity: String,
    headline: String,
    next_action: String,
  )
}

/// Typed workflow failures.
pub type WorkflowError {
  TriageFailed(message: String)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  Incident,
  TriageSummary,
  WorkflowError,
) {
  workflow.define(
    "incident_triage",
    incident_codec(),
    triage_summary_codec(),
    workflow_error_codec(),
    execute,
  )
}

/// Engine entry point.
///
/// The runtime delivers the start input as a raw JSON string: decode it with
/// the input codec, run the typed workflow, and encode the success value back
/// to its JSON string for the recorded result payload.
pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case incident_codec().decode(raw_json) {
        Ok(incident) ->
          case execute(incident) {
            Ok(summary) -> Ok(triage_summary_codec().encode(summary))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(TriageFailed("failed to decode incident input: " <> reason))
      }
    Error(_) -> Error(TriageFailed("workflow input payload was not a string"))
  }
}

/// The workflow body: one recorded `triage` activity dispatch.
fn execute(incident: Incident) -> Result(TriageSummary, WorkflowError) {
  case workflow.run(triage_activity(incident)) {
    Ok(summary) -> Ok(summary)
    Error(activity_error) ->
      Error(TriageFailed(activity_error_message(activity_error)))
  }
}

fn triage_activity(
  incident: Incident,
) -> activity.Activity(Incident, TriageSummary) {
  activity.new(
    "triage",
    incident,
    incident_codec(),
    triage_summary_codec(),
    local_triage,
  )
}

/// The in-VM fallback body. The activity carries no `execution_tier`, so it
/// dispatches on the remote wire and the Rust worker serves it; this local
/// runner keeps the typed `activity.new` contract satisfied and mirrors the
/// worker's logic exactly.
fn local_triage(
  incident: Incident,
) -> Result(TriageSummary, error.ActivityError) {
  Ok(TriageSummary(
    incident_id: incident.id,
    severity: incident.severity,
    headline: "[" <> incident.severity <> "] " <> incident.title,
    next_action: next_action_for(incident.severity),
  ))
}

/// Severity -> next action. Plain, deterministic string logic.
fn next_action_for(severity: String) -> String {
  case severity {
    "sev1" -> "page on-call and open a war room now"
    "sev2" -> "assign an owner and fix within the working day"
    "sev3" -> "triage into the backlog for the next sprint"
    _ -> "clarify severity before routing"
  }
}

// --- Codecs -----------------------------------------------------------------

fn incident_codec() -> codec.Codec(Incident) {
  codec.json_codec(incident_to_json, incident_decoder())
}

fn incident_to_json(incident: Incident) -> json.Json {
  json.object([
    #("id", json.string(incident.id)),
    #("title", json.string(incident.title)),
    #("severity", json.string(incident.severity)),
    #("observed", json.string(incident.observed)),
    #("expected", json.string(incident.expected)),
    #("environment", environment_to_json(incident.environment)),
    #("state", json.string(incident.state)),
  ])
}

fn incident_decoder() -> decode.Decoder(Incident) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use severity <- decode.field("severity", decode.string)
  use observed <- decode.field("observed", decode.string)
  use expected <- decode.field("expected", decode.string)
  use environment <- decode.field("environment", environment_decoder())
  use state <- decode.field("state", decode.string)
  decode.success(Incident(
    id: id,
    title: title,
    severity: severity,
    observed: observed,
    expected: expected,
    environment: environment,
    state: state,
  ))
}

fn environment_to_json(environment: Environment) -> json.Json {
  json.object([
    #("binary", json.string(environment.binary)),
    #("model", json.nullable(environment.model, json.string)),
    #("invocation", json.string(environment.invocation)),
  ])
}

fn environment_decoder() -> decode.Decoder(Environment) {
  use binary <- decode.field("binary", decode.string)
  use model <- decode.optional_field(
    "model",
    None,
    decode.map(decode.string, Some),
  )
  use invocation <- decode.field("invocation", decode.string)
  decode.success(Environment(
    binary: binary,
    model: model,
    invocation: invocation,
  ))
}

fn triage_summary_codec() -> codec.Codec(TriageSummary) {
  codec.json_codec(triage_summary_to_json, triage_summary_decoder())
}

fn triage_summary_to_json(summary: TriageSummary) -> json.Json {
  json.object([
    #("incident_id", json.string(summary.incident_id)),
    #("severity", json.string(summary.severity)),
    #("headline", json.string(summary.headline)),
    #("next_action", json.string(summary.next_action)),
  ])
}

fn triage_summary_decoder() -> decode.Decoder(TriageSummary) {
  use incident_id <- decode.field("incident_id", decode.string)
  use severity <- decode.field("severity", decode.string)
  use headline <- decode.field("headline", decode.string)
  use next_action <- decode.field("next_action", decode.string)
  decode.success(TriageSummary(
    incident_id: incident_id,
    severity: severity,
    headline: headline,
    next_action: next_action,
  ))
}

fn workflow_error_codec() -> codec.Codec(WorkflowError) {
  codec.json_codec(workflow_error_to_json, workflow_error_decoder())
}

fn workflow_error_to_json(err: WorkflowError) -> json.Json {
  case err {
    TriageFailed(message: message) ->
      json.object([#("triage_failed", json.string(message))])
  }
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use message <- decode.field("triage_failed", decode.string)
  decode.success(TriageFailed(message: message))
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}
