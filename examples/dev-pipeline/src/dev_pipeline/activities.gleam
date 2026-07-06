//// Typed activity values for the dev-pipeline workflows.
////
//// Every activity name built here is declared in the `activities` list of
//// its `workflow.toml` entry (`brief_forge` or `implement_and_gate`); the
//// final argument to each `activity.new` is the local seam from
//// `dev_pipeline/locals`.
////
//// Queues follow the doctrine substrate note: agent rounds (norn sessions)
//// dispatch on `agents`; workspace-bound command steps (provision, gates,
//// teardown) dispatch on `workspaces`, served by workers that carry the
//// repo toolchain. When the workflow input pins a `node`, every
//// implement-and-gate step is routed to the node holding the workspace
//// (`activity.node`) — an isolated worktree/clone lives on one node's disk.

import aion/activity
import dev_pipeline/codecs
import dev_pipeline/locals
import dev_pipeline/types.{
  type GateCliRun, type GateRun, type ImplementRound, type ImplementationReport,
  type ProvisionInput, type TeardownInput, type TornDown, type Workspace,
}
import gleam/option.{type Option, None, Some}

/// The task queue the agent (norn) activities dispatch on.
pub const agents_task_queue = "agents"

/// The task queue the workspace-bound command activities dispatch on —
/// served by workers that have the repo toolchain, not just norn.
pub const workspaces_task_queue = "workspaces"

/// Activity name served by the scout (norn run) worker handler.
pub const scout_name = "scout"

/// Activity name served by the designer (norn run) worker handler.
pub const design_name = "design"

/// Activity name served by the refuter (norn run) worker handler.
pub const refute_name = "refute"

/// `scout`: the grounding recon round, driven through the worker's norn
/// harness. The input IS the projected prompt; the harness derives the norn
/// session id (`{workflow_id}-scout`) at spawn. Output validates against
/// `schemas/scout-report.schema.json`.
pub fn scout(prompt: String) -> activity.Activity(String, types.ScoutReport) {
  activity.new(
    scout_name,
    prompt,
    codecs.prompt_codec(),
    codecs.scout_report_codec(),
    locals.scout,
  )
  |> activity.task_queue(agents_task_queue)
}

/// `design`: the brief-drafting round, driven through the worker's norn
/// harness. The harness session id (`{workflow_id}-design`, resumed via
/// `--resume-if-exists`) keeps the designer's own context across refute-loop
/// rounds. Output validates against `schemas/brief.schema.json`.
pub fn design(prompt: String) -> activity.Activity(String, types.Brief) {
  activity.new(
    design_name,
    prompt,
    codecs.prompt_codec(),
    codecs.brief_codec(),
    locals.design,
  )
  |> activity.task_queue(agents_task_queue)
}

/// `refute`: the attack round, driven through the worker's norn harness —
/// the refuter sees artifacts, not the designer's reasoning (the prompt
/// projection owns that hygiene). Harness session id is
/// `{workflow_id}-refute`, so loop rounds within one run RESUME one refuter
/// session (the per-round-fresh-session shell behaviour is not expressible
/// as a spawn template yet — a documented driven-mode deviation). Output
/// validates against `schemas/refutation.schema.json`.
pub fn refute(prompt: String) -> activity.Activity(String, types.Refutation) {
  activity.new(
    refute_name,
    prompt,
    codecs.prompt_codec(),
    codecs.refutation_codec(),
    locals.refute,
  )
  |> activity.task_queue(agents_task_queue)
}

// --- implement-and-gate ---------------------------------------------------------

/// Activity name served by the workspace-provisioning worker handler.
pub const provision_workspace_name = "provision_workspace"

/// Activity name served by the implementer (norn run) worker handler.
pub const implement_name = "implement"

/// Activity name served by the gate-command worker handler.
pub const run_gate_name = "run_gate"

/// Activity name served by the implementer-resume (norn resume) handler.
pub const implement_resume_name = "implement_resume"

/// Activity name served by the workspace-teardown worker handler.
pub const teardown_workspace_name = "teardown_workspace"

/// `provision_workspace`: create the isolated worktree/clone of `repo_root`
/// at `base_ref` under a scratch path. Failure is terminal — nothing
/// downstream can run without the workspace.
pub fn provision_workspace(
  input: ProvisionInput,
  node: Option(String),
) -> activity.Activity(ProvisionInput, Workspace) {
  activity.new(
    provision_workspace_name,
    input,
    codecs.provision_input_codec(),
    codecs.workspace_codec(),
    locals.provision_workspace,
  )
  |> activity.task_queue(workspaces_task_queue)
  |> with_node(node)
}

/// `implement`: the implementer round — norn run INSIDE the workspace in its
/// deterministic `<task_ref>-implement` session. Output validates against
/// `schemas/implementation-report.schema.json`.
pub fn implementer(
  round: ImplementRound,
  node: Option(String),
) -> activity.Activity(ImplementRound, ImplementationReport) {
  activity.new(
    implement_name,
    round,
    codecs.implement_round_codec(),
    codecs.implementation_report_codec(),
    locals.implementer,
  )
  |> activity.task_queue(agents_task_queue)
  |> activity.label("session", round.session_id)
  |> with_node(node)
}

/// `run_gate`: shell one gate command in the workspace and record
/// `{exit_status, output, duration_ms}` — a non-zero exit is DATA routed to
/// the fix loop, never an activity error; only a missing binary/workspace is
/// terminal. No pipe ever sits in the judged position; no agent ever
/// certifies a gate.
pub fn run_gate(
  gate_run: GateRun,
  node: Option(String),
) -> activity.Activity(GateRun, GateCliRun) {
  activity.new(
    run_gate_name,
    gate_run,
    codecs.gate_run_codec(),
    codecs.gate_cli_run_codec(),
    locals.run_gate,
  )
  |> activity.task_queue(workspaces_task_queue)
  |> activity.label("gate", gate_run.gate_id)
  |> with_node(node)
}

/// `implement_resume`: resume the SAME implementer session
/// (`<task_ref>-implement`) with a failing gate's captured output as the
/// feedback prompt. Returns a FULL replacement implementation report.
pub fn implement_resume(
  round: ImplementRound,
  node: Option(String),
) -> activity.Activity(ImplementRound, ImplementationReport) {
  activity.new(
    implement_resume_name,
    round,
    codecs.implement_round_codec(),
    codecs.implementation_report_codec(),
    locals.implement_resume,
  )
  |> activity.task_queue(agents_task_queue)
  |> activity.label("session", round.session_id)
  |> with_node(node)
}

/// `teardown_workspace`: best-effort workspace reclamation. Declared for
/// future wiring; the implement_and_gate workflow deliberately never
/// dispatches it — both termini preserve the workspace for inspection (see
/// the workflow's module doc).
pub fn teardown_workspace(
  input: TeardownInput,
  node: Option(String),
) -> activity.Activity(TeardownInput, TornDown) {
  activity.new(
    teardown_workspace_name,
    input,
    codecs.teardown_input_codec(),
    codecs.torn_down_codec(),
    locals.teardown_workspace,
  )
  |> activity.task_queue(workspaces_task_queue)
  |> with_node(node)
}

/// Pin an activity to the node holding the workspace when the workflow
/// input names one; unpinned otherwise (single-node deployments).
fn with_node(
  built: activity.Activity(input, output),
  node: Option(String),
) -> activity.Activity(input, output) {
  case node {
    Some(name) -> activity.node(built, name)
    None -> built
  }
}
