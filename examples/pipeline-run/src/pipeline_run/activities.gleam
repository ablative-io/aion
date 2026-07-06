//// The typed activity constructors the parent and child workflows dispatch.
////
//// Eight activity types, all served by the pipeline-run worker on ONE distinct
//// task queue (`pipeline_run`) so the run never collides with other workers on
//// `default`:
////
//// - FOUR agent activities (`scout`, `plan`, `dev`, `review`) — driven Norn
////   agents. Their INPUT is the composed prompt string; their OUTPUT is the
////   schema-constrained structured result the driven harness returns. The
////   worker installs one per-role harness (system prompt + `--output-schema` +
////   `{workflow_id}`-derived session id); the workflow never names Norn.
//// - FOUR shell activities (`provision_workspace`, `gate`, `land`, `notify`) —
////   typed registry handlers running real git/cargo commands.
////
//// Every activity dispatches on the remote wire (no execution tier set); the
//// local runner is a guard that never runs in production and fails loudly if a
//// misconfiguration ever routed one in-VM.

import aion/activity.{type Activity}
import aion/error
import pipeline_run/codecs
import pipeline_run/types.{
  type DevReport, type GateInput, type GateOutcome, type LandInput,
  type LandOutcome, type NotifyInput, type NotifyOutcome, type ProvisionInput,
  type ReviewVerdict, type ScoutFindings, type StackPlan, type WorkspaceInfo,
}

/// The one task queue every pipeline-run activity is dispatched on. Distinct
/// from `default` so the pipeline-run worker never competes with the triage or
/// other demo workers for tasks.
pub const task_queue = "pipeline_run"

/// A local runner for a remote-only activity: it must never execute in-VM, so
/// it fails loudly rather than returning a plausible-looking empty result.
fn remote_only(
  name: String,
) -> fn(input) -> Result(output, error.ActivityError) {
  fn(_input) {
    Error(error.Terminal(
      message: name
        <> " is a remote-only pipeline-run activity and has no in-VM runner",
      details: "",
    ))
  }
}

fn on_queue(activity: Activity(i, o)) -> Activity(i, o) {
  activity.task_queue(activity, task_queue)
}

// --- agent activities ------------------------------------------------------

/// `scout`: a driven grounding pass returning [`ScoutFindings`].
pub fn scout(prompt: String) -> Activity(String, ScoutFindings) {
  activity.new(
    "scout",
    prompt,
    codecs.prompt_codec(),
    codecs.scout_findings_codec(),
    remote_only("scout"),
  )
  |> on_queue
}

/// `plan`: a driven decomposition returning the [`StackPlan`].
pub fn plan(prompt: String) -> Activity(String, StackPlan) {
  activity.new(
    "plan",
    prompt,
    codecs.prompt_codec(),
    codecs.stack_plan_codec(),
    remote_only("plan"),
  )
  |> on_queue
}

/// `dev`: a driven dev round returning a [`DevReport`]. The harness resumes the
/// per-unit `{workflow_id}-dev` session across rounds, so a resume round's
/// prompt need only carry the new feedback.
pub fn dev(prompt: String) -> Activity(String, DevReport) {
  activity.new(
    "dev",
    prompt,
    codecs.prompt_codec(),
    codecs.dev_report_codec(),
    remote_only("dev"),
  )
  |> on_queue
}

/// `review`: a driven adversarial review returning a [`ReviewVerdict`]. The
/// harness resumes the per-unit `{workflow_id}-review` session across rounds.
pub fn review(prompt: String) -> Activity(String, ReviewVerdict) {
  activity.new(
    "review",
    prompt,
    codecs.prompt_codec(),
    codecs.review_verdict_codec(),
    remote_only("review"),
  )
  |> on_queue
}

// --- shell activities ------------------------------------------------------

/// `provision_workspace`: create the unit's isolated workspace, branching on
/// the prior unit's landed branch (or the integration base).
pub fn provision(
  input: ProvisionInput,
) -> Activity(ProvisionInput, WorkspaceInfo) {
  activity.new(
    "provision_workspace",
    input,
    codecs.provision_input_codec(),
    codecs.workspace_info_codec(),
    remote_only("provision_workspace"),
  )
  |> on_queue
}

/// `gate`: the cargo gate (clippy -D warnings, then test) in the workspace. A
/// non-zero cargo exit is recorded pass/fail DATA, never an activity error.
pub fn gate(input: GateInput) -> Activity(GateInput, GateOutcome) {
  activity.new(
    "gate",
    input,
    codecs.gate_input_codec(),
    codecs.gate_outcome_codec(),
    remote_only("gate"),
  )
  |> on_queue
}

/// `land`: merge the unit branches, in dependency order, onto the integration
/// branch, with a freshness re-check.
pub fn land(input: LandInput) -> Activity(LandInput, LandOutcome) {
  activity.new(
    "land",
    input,
    codecs.land_input_codec(),
    codecs.land_outcome_codec(),
    remote_only("land"),
  )
  |> on_queue
}

/// `notify`: a best-effort completion notice (logged, and sent to the
/// collective if the CLI is available).
pub fn notify(input: NotifyInput) -> Activity(NotifyInput, NotifyOutcome) {
  activity.new(
    "notify",
    input,
    codecs.notify_input_codec(),
    codecs.notify_outcome_codec(),
    remote_only("notify"),
  )
  |> on_queue
}
