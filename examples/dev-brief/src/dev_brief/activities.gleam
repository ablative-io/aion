//// The typed activity constructors the dev-brief workflows dispatch.
////
//// Seven activity types, all served by the dev-brief worker on ONE distinct
//// task queue (`dev_brief`) so runs never collide with other workers on
//// `default`, each PINNED to the one worker connection that serves it via
//// the node routing dimension (namespace × task_queue × node):
////
//// - TWO agent activities (`developer`, `review_lens`) — driven Norn agents,
////   each pinned to its role's node. Their INPUT is the STRUCTURED context;
////   the worker assembles the final prompt as {role profile markdown} +
////   {this context JSON} and the driven harness returns the
////   schema-constrained structured result.
//// - FIVE shell activities (`provision_workspace`, `run_gates`,
////   `reset_workspace`, `verify_gates`, `cleanup_workspace`) — typed registry
////   handlers running real git and the brief's configured gate commands,
////   pinned to the `shell` node.
////
//// The node pin is LOAD-BEARING (the remediation flow's lesson, kept): the
//// server routes a pushed activity to a worker connection by (namespace,
//// task_queue, node) ONLY — never by activity type. The worker opens THREE
//// connections on this one task queue, so without the pin an activity
//// round-robins onto a connection with no handler for it, never reports,
//// and dies at the heartbeat window as a lost-worker failure.
////
//// Every activity dispatches on the remote wire; the local runner is a
//// guard that never runs in production and fails loudly if a
//// misconfiguration ever routed one in-VM.

import aion/activity.{type Activity}
import aion/error
import dev_brief/codecs
import dev_brief/types.{
  type CleanupInput, type CleanupOutcome, type DevReport, type DeveloperInput,
  type GateInput, type GateOutcome, type LensInput, type LensVerdict,
  type ProvisionInput, type ResetInput, type ResetOutcome, type VerifyInput,
  type VerifyOutcome, type WorkspaceInfo,
}

/// The one task queue every dev-brief activity is dispatched on.
pub const task_queue = "dev_brief"

// --- node ids (the routing SOURCE OF TRUTH) -----------------------------------
//
// These three constants are THE authoritative node-id table for the dev-brief
// flow. The worker (worker/src/main.rs) registers each of its three liminal
// connections with exactly one of these ids. The strings MUST match exactly —
// the server routes on them blindly, so a mismatch strands the activity on a
// connection that cannot serve it.

/// The node the worker's SHELL connection registers; every shell activity
/// (`provision_workspace`, `run_gates`, `cleanup_workspace`) pins to it.
pub const shell_node = "shell"

/// The node of the `developer` driven-agent connection.
pub const developer_node = "developer"

/// The node of the `review_lens` driven-agent connection (one connection
/// serves every lens; the lens charter travels in the prompt, and each lens
/// child's own workflow id keys its session).
pub const reviewer_node = "reviewer"

/// A local runner for a remote-only activity: it must never execute in-VM,
/// so it fails loudly rather than returning a plausible-looking empty result.
fn remote_only(
  name: String,
) -> fn(input) -> Result(output, error.ActivityError) {
  fn(_input) {
    Error(error.Terminal(
      message: name
        <> " is a remote-only dev-brief activity and has no in-VM runner",
      details: "",
    ))
  }
}

/// Pin an activity to the dev-brief queue AND to the one worker connection
/// that serves it. Both dimensions are required: the queue keeps the run off
/// `default`, and the node selects the single connection (of the worker's
/// three on this queue) holding the handler — the server never routes by
/// activity type.
fn route(activity: Activity(i, o), node: String) -> Activity(i, o) {
  activity.node(activity.task_queue(activity, task_queue), node)
}

// --- agent activities ---------------------------------------------------------

/// `developer`: a driven implementation round returning the [`DevReport`].
/// The harness resumes the per-brief `{workflow_id}-developer` session across
/// cycles, so a loop-back round carries only the new gate/verdict feedback in
/// context.
pub fn developer(input: DeveloperInput) -> Activity(DeveloperInput, DevReport) {
  activity.new(
    "developer",
    input,
    codecs.developer_input_codec(),
    codecs.dev_report_codec(),
    remote_only("developer"),
  )
  |> route(developer_node)
}

/// `review_lens`: one driven adversarial review through one lens, returning
/// the [`LensVerdict`]. Dispatched by the `review_lens` CHILD workflow, so
/// concurrent lenses ride sibling child workflows.
pub fn review_lens(input: LensInput) -> Activity(LensInput, LensVerdict) {
  activity.new(
    "review_lens",
    input,
    codecs.lens_input_codec(),
    codecs.lens_verdict_codec(),
    remote_only("review_lens"),
  )
  |> route(reviewer_node)
}

// --- shell activities ------------------------------------------------------------

/// `provision_workspace`: create the brief's isolated git worktree, branched
/// on the configured base, returning the base commit the gate diffs against.
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
  |> route(shell_node)
}

/// `run_gates`: execute the brief's configured gate commands, exit status as
/// recorded data, plus the diff the reviewers read.
pub fn run_gates(input: GateInput) -> Activity(GateInput, GateOutcome) {
  activity.new(
    "run_gates",
    input,
    codecs.gate_input_codec(),
    codecs.gate_outcome_codec(),
    remote_only("run_gates"),
  )
  |> route(shell_node)
}

/// `cleanup_workspace`: remove the brief's worktree, refusing (honestly, as
/// recorded data) when it holds uncommitted work.
pub fn cleanup(input: CleanupInput) -> Activity(CleanupInput, CleanupOutcome) {
  activity.new(
    "cleanup_workspace",
    input,
    codecs.cleanup_input_codec(),
    codecs.cleanup_outcome_codec(),
    remote_only("cleanup_workspace"),
  )
  |> route(shell_node)
}

/// `reset_workspace`: the mechanical post-review restore (`git clean -fd` +
/// `git checkout -- .`) run after EVERY lens round, re-establishing the
/// worktree's exclusivity before the next developer round or the verify
/// stage. Guarded to a path strictly under the repo's dev-brief worktree
/// root; droppings are recorded, never fatal.
pub fn reset(input: ResetInput) -> Activity(ResetInput, ResetOutcome) {
  activity.new(
    "reset_workspace",
    input,
    codecs.reset_input_codec(),
    codecs.reset_outcome_codec(),
    remote_only("reset_workspace"),
  )
  |> route(shell_node)
}

/// `verify_gates`: the post-accept verification battery, re-run in the clean
/// workspace before cleanup. Recorded evidence only — a red gate here never
/// loops the developer back and never changes the disposition.
pub fn verify_gates(
  input: VerifyInput,
) -> Activity(VerifyInput, VerifyOutcome) {
  activity.new(
    "verify_gates",
    input,
    codecs.verify_input_codec(),
    codecs.verify_outcome_codec(),
    remote_only("verify_gates"),
  )
  |> route(shell_node)
}
