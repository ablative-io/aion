//// Typed activity values for the brief-forge workflow.
////
//// Every activity name built here is declared in the `activities` list of
//// the `brief_forge` entry in `workflow.toml`. All three dispatch on the
//// `agents` task queue (doctrine substrate note: three agent activities on
//// the dev-pipeline namespace, `activity.new(...) |>
//// activity.task_queue("agents")`); the final argument to each
//// `activity.new` is the local seam from `dev_pipeline/locals`.

import aion/activity
import dev_pipeline/codecs
import dev_pipeline/locals
import dev_pipeline/types.{type AgentRound}

/// The task queue all three agent activities dispatch on.
pub const agents_task_queue = "agents"

/// Activity name served by the scout (norn run) worker handler.
pub const scout_name = "scout"

/// Activity name served by the designer (norn run) worker handler.
pub const design_name = "design"

/// Activity name served by the refuter (norn run) worker handler.
pub const refute_name = "refute"

/// `scout`: the grounding recon round in its own deterministic norn session
/// (`<task_ref>-scout`). Output validates against
/// `schemas/scout-report.schema.json`.
pub fn scout(
  round: AgentRound,
) -> activity.Activity(AgentRound, types.ScoutReport) {
  activity.new(
    scout_name,
    round,
    codecs.agent_round_codec(),
    codecs.scout_report_codec(),
    locals.scout,
  )
  |> activity.task_queue(agents_task_queue)
  |> activity.label("session", round.session_id)
}

/// `design`: the brief-drafting round in its own deterministic norn session
/// (`<task_ref>-design`, resumed across refute-loop rounds so the designer
/// keeps its own context). Output validates against
/// `schemas/brief.schema.json`.
pub fn design(round: AgentRound) -> activity.Activity(AgentRound, types.Brief) {
  activity.new(
    design_name,
    round,
    codecs.agent_round_codec(),
    codecs.brief_codec(),
    locals.design,
  )
  |> activity.task_queue(agents_task_queue)
  |> activity.label("session", round.session_id)
}

/// `refute`: the attack round in a FRESH session per loop round
/// (`<task_ref>-refute-r<N>`) — the refuter sees artifacts, not the
/// designer's reasoning. Output validates against
/// `schemas/refutation.schema.json`.
pub fn refute(
  round: AgentRound,
) -> activity.Activity(AgentRound, types.Refutation) {
  activity.new(
    refute_name,
    round,
    codecs.agent_round_codec(),
    codecs.refutation_codec(),
    locals.refute,
  )
  |> activity.task_queue(agents_task_queue)
  |> activity.label("session", round.session_id)
}
